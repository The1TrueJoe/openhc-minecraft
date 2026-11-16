/*
 * capture — the video half of mc-record, inside the game's frame swap.
 *
 * mc-record starts ffmpeg on a FIFO and writes "W H FPS" to /tmp/mc-rec/on.
 * While that file exists, every frame due at FPS is:
 *   1. copied on the GPU from the back buffer into a texture;
 *   2. drawn by a shader into an RGBA texture W/4 wide and H*3/2 high whose
 *      bytes, read in order, are the frame as I420 at WxH (BT.601, TV range):
 *      the GPU does the scaling and the colour conversion, and the CPU reads
 *      back 1.5 bytes a pixel instead of 4;
 *   3. read back two captures later, from a ring of three such targets: a
 *      readback waits for its render to finish (glReadPixels does, on SGX,
 *      even into a pixel-pack buffer), and by then that render is long done,
 *      so the game never waits. A worker thread writes the frame to the FIFO;
 *      a frame it has no time for is dropped, not waited for.
 * Removing the file ends the recording: the FIFO closes and ffmpeg finishes.
 *
 * GL state the game had is put back: everything here is inside
 * glPushAttrib/glPushClientAttrib, plus the bindings those do not cover.
 */
#define _GNU_SOURCE
#include <GL/gl.h>
#include <GL/glext.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "capture.h"

#define REC_ON "/tmp/mc-rec/on"
#define REC_FIFO "/tmp/mc-rec/video"
#define SLOTS 3			/* pack targets in the ring; read SLOTS-1 captures late */

/* GL 2.0 and framebuffer-object entry points, from the real libGL */
static PFNGLCREATESHADERPROC CreateShader;
static PFNGLSHADERSOURCEPROC ShaderSource;
static PFNGLCOMPILESHADERPROC CompileShader;
static PFNGLGETSHADERIVPROC GetShaderiv;
static PFNGLGETSHADERINFOLOGPROC GetShaderInfoLog;
static PFNGLCREATEPROGRAMPROC CreateProgram;
static PFNGLATTACHSHADERPROC AttachShader;
static PFNGLLINKPROGRAMPROC LinkProgram;
static PFNGLGETPROGRAMIVPROC GetProgramiv;
static PFNGLUSEPROGRAMPROC UseProgram;
static PFNGLDELETEPROGRAMPROC DeleteProgram;
static PFNGLDELETESHADERPROC DeleteShader;
static PFNGLGETUNIFORMLOCATIONPROC GetUniformLocation;
static PFNGLUNIFORM1IPROC Uniform1i;
static PFNGLUNIFORM2FPROC Uniform2f;
static PFNGLACTIVETEXTUREPROC ActiveTexture;
static PFNGLGENFRAMEBUFFERSEXTPROC GenFramebuffers;
static PFNGLBINDFRAMEBUFFEREXTPROC BindFramebuffer;
static PFNGLFRAMEBUFFERTEXTURE2DEXTPROC FramebufferTexture2D;
static PFNGLCHECKFRAMEBUFFERSTATUSEXTPROC CheckFramebufferStatus;
static PFNGLDELETEFRAMEBUFFERSEXTPROC DeleteFramebuffers;
static PFNGLBINDBUFFERPROC BindBuffer;

typedef void (*querydrawable_t)(void *, unsigned long, int, unsigned int *);
#define GLX_WIDTH 0x801D
#define GLX_HEIGHT 0x801E

static void *proc(const char *name)
{
	static void *(*gpa)(const GLubyte *);
	if (!gpa)
		gpa = (void *(*)(const GLubyte *))dlsym(RTLD_NEXT, "glXGetProcAddressARB");
	return gpa ? gpa((const GLubyte *)name) : NULL;
}

/* ARB names first; the EXT framebuffer-object names on drivers without them */
static void *proc2(const char *name, const char *ext)
{
	void *p = proc(name);
	return p ? p : proc(ext);
}

static int load(void)
{
#define P(v, n) if (!(v = proc(n))) return -1
#define P2(v, n, e) if (!(v = proc2(n, e))) return -1
	P(CreateShader, "glCreateShader"); P(ShaderSource, "glShaderSource");
	P(CompileShader, "glCompileShader"); P(GetShaderiv, "glGetShaderiv");
	P(GetShaderInfoLog, "glGetShaderInfoLog"); P(CreateProgram, "glCreateProgram");
	P(AttachShader, "glAttachShader"); P(LinkProgram, "glLinkProgram");
	P(GetProgramiv, "glGetProgramiv"); P(UseProgram, "glUseProgram");
	P(DeleteProgram, "glDeleteProgram"); P(DeleteShader, "glDeleteShader");
	P(GetUniformLocation, "glGetUniformLocation"); P(Uniform1i, "glUniform1i");
	P(Uniform2f, "glUniform2f"); P(ActiveTexture, "glActiveTexture");
	P2(GenFramebuffers, "glGenFramebuffers", "glGenFramebuffersEXT");
	P2(BindFramebuffer, "glBindFramebuffer", "glBindFramebufferEXT");
	P2(FramebufferTexture2D, "glFramebufferTexture2D", "glFramebufferTexture2DEXT");
	P2(CheckFramebufferStatus, "glCheckFramebufferStatus", "glCheckFramebufferStatusEXT");
	P2(DeleteFramebuffers, "glDeleteFramebuffers", "glDeleteFramebuffersEXT");
	P(BindBuffer, "glBindBuffer");
	return 0;
#undef P
#undef P2
}

/*
 * The target is size.x/4 texels by size.y*3/2, row 0 at the bottom of the
 * framebuffer and so first in the readback: rows 0..H-1 hold Y, then H/4 rows
 * of U and H/4 of V, each texel four consecutive bytes of its plane.
 */
static const char *vs =
	"#version 120\n"
	"void main() { gl_Position = gl_Vertex; }\n";
static const char *fs =
	"#version 120\n"
	"uniform sampler2D src;\n"
	"uniform vec2 size;\n"
	"float Y(vec3 c) { return (16.0 + 65.481 * c.r + 128.553 * c.g + 24.966 * c.b) / 255.0; }\n"
	"float U(vec3 c) { return (128.0 - 37.797 * c.r - 74.203 * c.g + 112.0 * c.b) / 255.0; }\n"
	"float V(vec3 c) { return (128.0 + 112.0 * c.r - 93.786 * c.g - 18.214 * c.b) / 255.0; }\n"
	/* the shown frame, top row first */
	"vec3 at(vec2 p) { return texture2D(src, vec2(p.x / size.x, 1.0 - p.y / size.y)).rgb; }\n"
	"void main() {\n"
	"  vec2 f = floor(gl_FragCoord.xy);\n"
	"  float x = f.x * 4.0;\n"
	"  if (f.y < size.y) {\n"
	"    float y = f.y + 0.5;\n"
	"    gl_FragColor = vec4(Y(at(vec2(x + 0.5, y))), Y(at(vec2(x + 1.5, y))),\n"
	"                        Y(at(vec2(x + 2.5, y))), Y(at(vec2(x + 3.5, y))));\n"
	"  } else {\n"
	"    float q = f.y - size.y, rows = size.y / 4.0;\n"
	"    float v = step(rows, q);\n"
	"    float b = (q - v * rows) * size.x + x;\n"
	"    float cw = size.x / 2.0;\n"
	"    float cy = floor(b / cw), cx = b - cy * cw;\n"
	/* a chroma sample is the middle of its 2x2 block */
	"    vec3 c0 = at(vec2(2.0 * cx + 1.0, 2.0 * cy + 1.0)), c1 = at(vec2(2.0 * cx + 3.0, 2.0 * cy + 1.0));\n"
	"    vec3 c2 = at(vec2(2.0 * cx + 5.0, 2.0 * cy + 1.0)), c3 = at(vec2(2.0 * cx + 7.0, 2.0 * cy + 1.0));\n"
	"    gl_FragColor = v < 0.5 ? vec4(U(c0), U(c1), U(c2), U(c3)) : vec4(V(c0), V(c1), V(c2), V(c3));\n"
	"  }\n"
	"}\n";

static struct {
	int on, broken;
	int w, h, fps;
	unsigned sw, sh;		/* source: the window */
	GLuint src, dst[SLOTS], fbo[SLOTS], prog;
	GLint u_size, u_src;
	int packed[SLOTS];		/* holds a frame not read back yet */
	int k;				/* the target the next capture packs into */
	unsigned char *rd;		/* readback, handed to the worker */
	int fd;
	size_t size;
	int room;			/* the FIFO's capacity when it holds a frame, else 0 */
	double next;			/* when the next frame is due */
	unsigned long taken, dropped;
	pthread_t worker;
} c = { .fd = -1 };

/* the worker: one frame in hand at a time */
static struct {
	pthread_mutex_t m;
	pthread_cond_t cv;
	const unsigned char *in;	/* c.rd while it holds a frame to copy, or NULL */
	int quit;
	unsigned char *buf;
} w = { PTHREAD_MUTEX_INITIALIZER, PTHREAD_COND_INITIALIZER, NULL, 0, NULL };

static void *work(void *arg)
{
	(void)arg;
	for (;;) {
		pthread_mutex_lock(&w.m);
		while (!w.in && !w.quit)
			pthread_cond_wait(&w.cv, &w.m);
		if (!w.in) {
			pthread_mutex_unlock(&w.m);
			break;
		}
		const unsigned char *in = w.in;
		pthread_mutex_unlock(&w.m);

		/* Only a whole frame goes in: one the encoder has no room for yet is
		 * dropped here, before it is copied, never cut short. */
		int queued = 0;
		if (c.room && ioctl(c.fd, FIONREAD, &queued) == 0 && (size_t)(c.room - queued) < c.size) {
			pthread_mutex_lock(&w.m);
			w.in = NULL;
			c.dropped++;
			pthread_mutex_unlock(&w.m);
			continue;
		}
		memcpy(w.buf, in, c.size);

		pthread_mutex_lock(&w.m);
		w.in = NULL;		/* c.rd is free for the next readback */
		pthread_mutex_unlock(&w.m);

		for (size_t off = 0; off < c.size;) {
			/* there was room, so this only waits on a pipe smaller than a
			 * frame; ffmpeg stuck for 10 s counts as gone */
			struct pollfd pf = { c.fd, POLLOUT, 0 };
			ssize_t n = poll(&pf, 1, 10000) == 1 ? write(c.fd, w.buf + off, c.size - off) : -1;
			if (n < 0 && errno == EINTR)
				continue;
			if (n <= 0) {	/* ffmpeg is gone: stop writing */
				pthread_mutex_lock(&w.m);
				w.quit = 1;
				pthread_mutex_unlock(&w.m);
				return NULL;
			}
			off += n;
		}
	}
	return NULL;
}

static double now(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return t.tv_sec + t.tv_nsec / 1e9;
}

static GLuint shader(GLenum type, const char *src)
{
	GLuint s = CreateShader(type);
	GLint ok = 0;
	ShaderSource(s, 1, &src, NULL);
	CompileShader(s);
	GetShaderiv(s, GL_COMPILE_STATUS, &ok);
	if (!ok) {
		char log[512] = "";
		GetShaderInfoLog(s, sizeof log, NULL, log);
		fprintf(stderr, "mc-record: shader: %s\n", log);
		DeleteShader(s);
		return 0;
	}
	return s;
}

static void teardown(void)
{
	if (c.prog)
		DeleteProgram(c.prog);
	DeleteFramebuffers(SLOTS, c.fbo);
	glDeleteTextures(1, &c.src);
	glDeleteTextures(SLOTS, c.dst);
	for (int i = 0; i < SLOTS; i++)
		c.dst[i] = c.fbo[i] = c.packed[i] = 0;
	free(c.rd);
	c.rd = NULL;
	c.src = c.prog = 0;
}

/* GL objects for a recording at c.w x c.h from a c.sw x c.sh window */
static int setup(void)
{
	GLuint v = shader(GL_VERTEX_SHADER, vs), f = shader(GL_FRAGMENT_SHADER, fs);
	GLint ok = 0;
	if (!v || !f)
		return -1;
	c.prog = CreateProgram();
	AttachShader(c.prog, v);
	AttachShader(c.prog, f);
	LinkProgram(c.prog);
	DeleteShader(v);
	DeleteShader(f);
	GetProgramiv(c.prog, GL_LINK_STATUS, &ok);
	if (!ok)
		return -1;
	c.u_size = GetUniformLocation(c.prog, "size");
	c.u_src = GetUniformLocation(c.prog, "src");

	glGenTextures(1, &c.src);
	glBindTexture(GL_TEXTURE_2D, c.src);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
	glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, c.sw, c.sh, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);

	glGenTextures(SLOTS, c.dst);
	GenFramebuffers(SLOTS, c.fbo);
	for (int i = 0; i < SLOTS; i++) {
		glBindTexture(GL_TEXTURE_2D, c.dst[i]);
		glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
		glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
		glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, c.w / 4, c.h * 3 / 2, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
		BindFramebuffer(GL_FRAMEBUFFER_EXT, c.fbo[i]);
		FramebufferTexture2D(GL_FRAMEBUFFER_EXT, GL_COLOR_ATTACHMENT0_EXT, GL_TEXTURE_2D, c.dst[i], 0);
		if (CheckFramebufferStatus(GL_FRAMEBUFFER_EXT) != GL_FRAMEBUFFER_COMPLETE_EXT)
			return -1;
		c.packed[i] = 0;
	}
	c.k = 0;
	c.rd = malloc(c.size);
	return c.rd && glGetError() == GL_NO_ERROR ? 0 : -1;
}

/* Ends the recording; called with GL state saved. */
static void stop(void)
{
	pthread_mutex_lock(&w.m);
	w.quit = 1;
	pthread_cond_signal(&w.cv);
	pthread_mutex_unlock(&w.m);
	pthread_join(c.worker, NULL);
	close(c.fd);
	c.fd = -1;
	teardown();
	free(w.buf);
	w.buf = NULL;
	fprintf(stderr, "mc-record: stopped, %lu frames, %lu dropped\n", c.taken, c.dropped);
	c.on = 0;
}

/* Starts one when mc-record has asked for it and its ffmpeg is reading. */
static int start(void *dpy, unsigned long drawable)
{
	static querydrawable_t qd;
	FILE *f = fopen(REC_ON, "r");
	int ok = f && fscanf(f, "%d %d %d", &c.w, &c.h, &c.fps) == 3;
	if (f)
		fclose(f);
	if (!ok || c.w < 64 || c.h < 64 || c.w > 1920 || c.h > 1080 || c.w % 8 || c.h % 4 ||
	    c.fps < 1 || c.fps > 60)
		return -1;
	if (!qd)
		qd = (querydrawable_t)dlsym(RTLD_NEXT, "glXQueryDrawable");
	c.sw = c.sh = 0;
	if (qd) {
		qd(dpy, drawable, GLX_WIDTH, &c.sw);
		qd(dpy, drawable, GLX_HEIGHT, &c.sh);
	}
	if (!c.sw || !c.sh)
		return -1;

	/* ffmpeg opens the FIFO for reading first; without it, try a later frame */
	c.fd = open(REC_FIFO, O_WRONLY | O_NONBLOCK | O_CLOEXEC);
	if (c.fd < 0)
		return 1;

	c.size = (size_t)c.w * c.h * 3 / 2;
	/* a FIFO big enough for two frames (the default 1 MiB limit allows 720x480) */
	if (fcntl(c.fd, F_SETPIPE_SZ, (int)(2 * c.size)) < 0 && fcntl(c.fd, F_SETPIPE_SZ, (int)c.size) < 0)
		fcntl(c.fd, F_SETPIPE_SZ, 1 << 20);
	int cap = fcntl(c.fd, F_GETPIPE_SZ);
	c.room = cap >= (int)c.size ? cap : 0;
	w.buf = malloc(c.size);
	if (!w.buf || setup()) {
		fprintf(stderr, "mc-record: no GPU capture (GL setup failed)\n");
		teardown();
		free(w.buf);
		w.buf = NULL;
		close(c.fd);
		c.fd = -1;
		return -1;
	}
	w.in = NULL;
	w.quit = 0;
	pthread_create(&c.worker, NULL, work, NULL);
	c.taken = c.dropped = 0;
	c.next = now();
	c.on = 1;
	fprintf(stderr, "mc-record: recording %dx%d at %d fps from %ux%u\n", c.w, c.h, c.fps, c.sw, c.sh);
	return 0;
}

/* One captured frame: GPU copy + convert into the ring, and the frame packed
 * two captures ago read back and handed to the worker. Called with GL state
 * saved. */
static void take(void)
{
	pthread_mutex_lock(&w.m);
	int quit = w.quit, busy = w.in != NULL;
	pthread_mutex_unlock(&w.m);
	if (quit)
		return;
	int k = c.k, old = (k + 1) % SLOTS;	/* = k - (SLOTS - 1) */

	/* 1. the shown frame into a texture */
	ActiveTexture(GL_TEXTURE0);
	BindFramebuffer(GL_FRAMEBUFFER_EXT, 0);	/* its read buffer is what is being drawn */
	glBindTexture(GL_TEXTURE_2D, c.src);
	glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 0, 0, c.sw, c.sh);

	/* 2. I420 into this capture's target */
	BindFramebuffer(GL_FRAMEBUFFER_EXT, c.fbo[k]);
	glViewport(0, 0, c.w / 4, c.h * 3 / 2);
	glDisable(GL_BLEND);
	glDisable(GL_DEPTH_TEST);
	glDisable(GL_SCISSOR_TEST);
	glDisable(GL_STENCIL_TEST);
	glDisable(GL_CULL_FACE);
	glDisable(GL_ALPHA_TEST);
	glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
	UseProgram(c.prog);
	Uniform1i(c.u_src, 0);
	Uniform2f(c.u_size, c.w, c.h);
	glBegin(GL_QUADS);
	glVertex2f(-1, -1);
	glVertex2f(1, -1);
	glVertex2f(1, 1);
	glVertex2f(-1, 1);
	glEnd();
	c.packed[k] = 1;
	c.k = old;
	c.taken++;

	/* 3. the oldest target: its render finished long ago, so this read does not
	 *    wait. Exactly RGBA/UNSIGNED_BYTE from RGBA8, into client memory. */
	if (!c.packed[old])
		return;
	c.packed[old] = 0;
	if (busy) {	/* the worker still has the last one */
		c.dropped++;
		return;
	}
	BindFramebuffer(GL_FRAMEBUFFER_EXT, c.fbo[old]);
	BindBuffer(GL_PIXEL_PACK_BUFFER, 0);
	glPixelStorei(GL_PACK_ALIGNMENT, 4);
	glPixelStorei(GL_PACK_ROW_LENGTH, 0);
	glReadPixels(0, 0, c.w / 4, c.h * 3 / 2, GL_RGBA, GL_UNSIGNED_BYTE, c.rd);
	pthread_mutex_lock(&w.m);
	w.in = c.rd;
	pthread_cond_signal(&w.cv);
	pthread_mutex_unlock(&w.m);
}

void capture_frame(void *dpy, unsigned long drawable)
{
	static int loaded, retry;
	struct stat st;
	int want = stat(REC_ON, &st) == 0;

	if (!want && !c.on) {
		c.broken = 0;	/* a new recording gets a new try */
		return;
	}
	if (c.broken)
		return;
	if (!loaded) {
		if (load()) {
			fprintf(stderr, "mc-record: this GL has no shaders/framebuffer objects\n");
			c.broken = 1;
			return;
		}
		loaded = 1;
	}

	double t = now();
	if (c.on && want && t < c.next)
		return;
	if (!c.on && want && retry && retry--)
		return;	/* waiting for ffmpeg: check a few frames apart */

	GLint prog, fbo, pbo;
	glGetIntegerv(GL_CURRENT_PROGRAM, &prog);
	glGetIntegerv(GL_FRAMEBUFFER_BINDING_EXT, &fbo);
	glGetIntegerv(GL_PIXEL_PACK_BUFFER_BINDING, &pbo);
	glPushAttrib(GL_ENABLE_BIT | GL_COLOR_BUFFER_BIT | GL_VIEWPORT_BIT | GL_TEXTURE_BIT |
		     GL_PIXEL_MODE_BIT | GL_SCISSOR_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
	glPushClientAttrib(GL_CLIENT_PIXEL_STORE_BIT);

	if (!c.on) {
		int r = start(dpy, drawable);
		if (r < 0)
			c.broken = 1;
		else if (r > 0)
			retry = 10;
	}
	if (c.on && (!want || w.quit)) {
		int gone = w.quit;	/* ffmpeg went away: stay off until the file goes */
		stop();
		c.broken = want && gone;
	}
	else if (c.on) {
		take();
		c.next += 1.0 / c.fps;
		if (c.next < t)	/* fell behind (a slow frame): don't catch up in a burst */
			c.next = t + 1.0 / c.fps;
	}

	glPopClientAttrib();
	glPopAttrib();
	BindBuffer(GL_PIXEL_PACK_BUFFER, pbo);
	BindFramebuffer(GL_FRAMEBUFFER_EXT, fbo);
	UseProgram(prog);
}
