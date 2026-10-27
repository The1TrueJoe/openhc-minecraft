/*
 * mc-overlay — draws what the game cannot on this box: the pointer while a menu
 * is up, and the on-screen keyboard a game controller types with.
 *
 * The game's window lives on a virtual X server (Xvfb); its picture reaches the
 * panel through gl4es in framebuffer mode, so the X cursor is never seen. This
 * library is what LWJGL loads as its libGL (-Dorg.lwjgl.opengl.libname). It
 * defines only glXSwapBuffers — and glXGetProcAddress*, through which LWJGL
 * looks that up — and resolves everything else to gl4es through DT_NEEDED. Just
 * before each frame is shown it draws over it, with plain GL 1.x calls into gl4es
 * so gl4es's own state tracking stays right, and puts back what it changed.
 *
 * What to draw comes from mc-inputd, which owns the state: /tmp/mc-ui (struct
 * mcui below; must match inputd/src/ui.rs, as must the keyboard layout).
 */
#define _GNU_SOURCE
#include <GL/gl.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <ft2build.h>
#include FT_FREETYPE_H
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

struct mcui {
	uint32_t magic, seq;
	int32_t w, h;
	int32_t cursor, x, y;
	int32_t osk, row, col, shift, top;
};
#define MCUI_MAGIC 0x4955434du
#define MCUI_PATH "/tmp/mc-ui"
#define FONT "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"

static const volatile struct mcui *ui;

static const struct mcui *state(void)
{
	static int tries;
	if (!ui && (tries++ % 120) == 0) { /* mc-inputd may start after us */
		int fd = open(MCUI_PATH, O_RDONLY);
		if (fd >= 0) {
			void *p = mmap(NULL, sizeof(struct mcui), PROT_READ, MAP_SHARED, fd, 0);
			close(fd);
			if (p != MAP_FAILED)
				ui = p;
		}
	}
	return ui && ui->magic == MCUI_MAGIC ? (const struct mcui *)ui : NULL;
}

/* ---- the pointer: an arrow, 1 outline, . fill, drawn at 2x ---- */
static const char *arrow[] = {
	"1           ",
	"11          ",
	"1.1         ",
	"1..1        ",
	"1...1       ",
	"1....1      ",
	"1.....1     ",
	"1......1    ",
	"1.......1   ",
	"1........1  ",
	"1.....11111 ",
	"1..1..1     ",
	"1.1 1..1    ",
	"11  1..1    ",
	"1    1..1   ",
	"     1..1   ",
	"      11    ",
};
#define ARROW_W 16
#define ARROW_H 32

/* ---- the keyboard; layout as in inputd/src/ui.rs ---- */
static const char *rows[2][4] = {
	{ "1234567890", "qwertyuiop", "asdfghjkl/", "zxcvbnm,.-" },
	{ "!@#$%^&*()", "QWERTYUIOP", "ASDFGHJKL?", "ZXCVBNM<>_" },
};
static const struct { const char *label; int col, span; } bottom[4] = {
	{ "Shift", 0, 2 }, { "Space", 2, 4 }, { "Del", 6, 2 }, { "Enter", 8, 2 },
};
static const char *legend = "A type   X delete   Y shift   B close   Start enter   LB/RB move";

/* ---- glyph atlas, rendered once from DejaVu with FreeType ---- */
#define ATLAS 256
static struct { int x, y, w, h, left, top, adv; } glyph[2][96]; /* [small, big][' '..'~'] */
static int line_h[2];
static GLuint tex_font, tex_arrow;
static int ready, failed;

static int bake(FT_Face face, int size, int which, unsigned char *atlas, int *px, int *py, int *rowh)
{
	FT_Set_Pixel_Sizes(face, 0, size);
	line_h[which] = size;
	for (int c = 32; c < 127; c++) {
		if (FT_Load_Char(face, c, FT_LOAD_RENDER))
			continue;
		FT_GlyphSlot g = face->glyph;
		int w = g->bitmap.width, h = g->bitmap.rows;
		if (*px + w + 1 > ATLAS) {
			*px = 0;
			*py += *rowh + 1;
			*rowh = 0;
		}
		if (*py + h > ATLAS)
			return -1;
		for (int y = 0; y < h; y++)
			memcpy(atlas + (*py + y) * ATLAS + *px, g->bitmap.buffer + y * g->bitmap.pitch, w);
		glyph[which][c - 32].x = *px;
		glyph[which][c - 32].y = *py;
		glyph[which][c - 32].w = w;
		glyph[which][c - 32].h = h;
		glyph[which][c - 32].left = g->bitmap_left;
		glyph[which][c - 32].top = g->bitmap_top;
		glyph[which][c - 32].adv = g->advance.x >> 6;
		*px += w + 1;
		if (h > *rowh)
			*rowh = h;
	}
	return 0;
}

static void setup(void)
{
	static unsigned char alpha[ATLAS * ATLAS], rgba[ATLAS * ATLAS * 4];
	static unsigned char apix[ARROW_W * ARROW_H * 4];
	FT_Library ft;
	FT_Face face;
	int px = 0, py = 0, rowh = 0;

	failed = 1;
	if (FT_Init_FreeType(&ft) || FT_New_Face(ft, FONT, 0, &face))
		return;
	if (bake(face, 11, 0, alpha, &px, &py, &rowh) || bake(face, 16, 1, alpha, &px, &py, &rowh))
		return;
	FT_Done_Face(face);
	FT_Done_FreeType(ft);
	for (int i = 0; i < ATLAS * ATLAS; i++) {
		rgba[i * 4] = rgba[i * 4 + 1] = rgba[i * 4 + 2] = 255;
		rgba[i * 4 + 3] = alpha[i];
	}
	for (int y = 0; y < (int)(sizeof arrow / sizeof *arrow); y++)
		for (int x = 0; arrow[y][x]; x++) {
			unsigned char *p = apix + (y * ARROW_W + x) * 4;
			if (arrow[y][x] == '1')
				p[3] = 255;
			else if (arrow[y][x] == '.')
				p[0] = p[1] = p[2] = p[3] = 255;
		}
	glGenTextures(1, &tex_font);
	glBindTexture(GL_TEXTURE_2D, tex_font);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
	glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, ATLAS, ATLAS, 0, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
	glGenTextures(1, &tex_arrow);
	glBindTexture(GL_TEXTURE_2D, tex_arrow);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
	glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, ARROW_W, ARROW_H, 0, GL_RGBA, GL_UNSIGNED_BYTE, apix);
	failed = 0;
	ready = 1;
}

static void rect(float x0, float y0, float x1, float y1, float r, float g, float b, float a)
{
	glDisable(GL_TEXTURE_2D);
	glColor4f(r, g, b, a);
	glBegin(GL_QUADS);
	glVertex2f(x0, y0);
	glVertex2f(x1, y0);
	glVertex2f(x1, y1);
	glVertex2f(x0, y1);
	glEnd();
}

static int text_width(const char *s, int which)
{
	int w = 0;
	for (; *s; s++)
		if (*s >= 32 && *s < 127)
			w += glyph[which][*s - 32].adv;
	return w;
}

/* Draw `s` with its baseline at y. */
static void text(const char *s, float x, float y, int which)
{
	glEnable(GL_TEXTURE_2D);
	glBindTexture(GL_TEXTURE_2D, tex_font);
	glColor4f(1, 1, 1, 1);
	glBegin(GL_QUADS);
	for (; *s; s++) {
		if (*s < 32 || *s >= 127)
			continue;
		const typeof(glyph[0][0]) *g = &glyph[which][*s - 32];
		float gx = x + g->left, gy = y - g->top;
		float u0 = (float)g->x / ATLAS, v0 = (float)g->y / ATLAS;
		float u1 = (float)(g->x + g->w) / ATLAS, v1 = (float)(g->y + g->h) / ATLAS;
		glTexCoord2f(u0, v0); glVertex2f(gx, gy);
		glTexCoord2f(u1, v0); glVertex2f(gx + g->w, gy);
		glTexCoord2f(u1, v1); glVertex2f(gx + g->w, gy + g->h);
		glTexCoord2f(u0, v1); glVertex2f(gx, gy + g->h);
		x += g->adv;
	}
	glEnd();
}

static void key(float x0, float y0, float x1, float y1, const char *label, int selected, int lit)
{
	if (selected)
		rect(x0, y0, x1, y1, 0.30f, 0.58f, 0.95f, 0.95f);
	else if (lit)
		rect(x0, y0, x1, y1, 0.42f, 0.42f, 0.42f, 0.92f);
	else
		rect(x0, y0, x1, y1, 0.18f, 0.18f, 0.18f, 0.88f);
	int tw = text_width(label, 1);
	text(label, (x0 + x1 - tw) / 2, (y0 + y1) / 2 + line_h[1] * 0.38f, 1);
}

static void draw_keyboard(const struct mcui *s)
{
	const float gap = 3, kh = 30, margin = 12;
	const float panel = 5 * kh + 4 * gap + 2 * margin + 18; /* keys + legend */
	float kw = (s->w - 2 * margin + gap) / 10.0f;
	/* at the bottom, or at the top for chat, whose input line is at the bottom */
	float y0 = s->top ? 0 : s->h - panel;
	float top = y0 + margin + 18;
	char cap[2] = { 0, 0 };

	rect(0, y0, s->w, y0 + panel, 0, 0, 0, 0.62f);
	text(legend, margin, top - 10, 0);
	for (int r = 0; r < 4; r++)
		for (int c = 0; c < 10; c++) {
			float x = margin + c * kw, y = top + r * (kh + gap);
			cap[0] = rows[s->shift ? 1 : 0][r][c];
			key(x, y, x + kw - gap, y + kh, cap, s->row == r && s->col == c, 0);
		}
	for (int k = 0; k < 4; k++) {
		float x = margin + bottom[k].col * kw, y = top + 4 * (kh + gap);
		int sel = s->row == 4 && s->col >= bottom[k].col && s->col < bottom[k].col + bottom[k].span;
		key(x, y, x + bottom[k].span * kw - gap, y + kh, bottom[k].label, sel, k == 0 && s->shift);
	}
}

static void draw_pointer(const struct mcui *s)
{
	glEnable(GL_TEXTURE_2D);
	glBindTexture(GL_TEXTURE_2D, tex_arrow);
	glColor4f(1, 1, 1, 1);
	float x = s->x, y = s->y, w = ARROW_W * 2, h = ARROW_H * 2;
	glBegin(GL_QUADS);
	glTexCoord2f(0, 0); glVertex2f(x, y);
	glTexCoord2f(1, 0); glVertex2f(x + w, y);
	glTexCoord2f(1, 1); glVertex2f(x + w, y + h);
	glTexCoord2f(0, 1); glVertex2f(x, y + h);
	glEnd();
}

/* Draw over the frame, leaving the GL state as the game had it. */
static void overlay(void)
{
	const struct mcui *s = state();
	if (!s || (!s->cursor && !s->osk) || s->w <= 0 || s->h <= 0 || failed)
		return;
	struct mcui st = *s; /* one consistent copy for this frame */

	GLint vp[4], mode, active, client, bound, src, dst, envmode;
	GLfloat color[4];
	GLboolean depthmask;
	static const GLenum caps[] = { GL_DEPTH_TEST, GL_CULL_FACE, GL_ALPHA_TEST, GL_FOG, GL_LIGHTING,
				       GL_BLEND, GL_SCISSOR_TEST, GL_COLOR_MATERIAL, GL_STENCIL_TEST };
	GLboolean was[sizeof caps / sizeof *caps], tex2d[2];

	glGetIntegerv(GL_VIEWPORT, vp);
	glGetIntegerv(GL_MATRIX_MODE, &mode);
	glGetIntegerv(GL_ACTIVE_TEXTURE, &active);
	glGetIntegerv(GL_CLIENT_ACTIVE_TEXTURE, &client);
	glGetFloatv(GL_CURRENT_COLOR, color);
	glGetBooleanv(GL_DEPTH_WRITEMASK, &depthmask);
	glGetIntegerv(GL_BLEND_SRC, &src);
	glGetIntegerv(GL_BLEND_DST, &dst);
	for (unsigned i = 0; i < sizeof caps / sizeof *caps; i++)
		was[i] = glIsEnabled(caps[i]);
	glActiveTexture(GL_TEXTURE1);
	tex2d[1] = glIsEnabled(GL_TEXTURE_2D);
	glDisable(GL_TEXTURE_2D);
	glActiveTexture(GL_TEXTURE0);
	glClientActiveTexture(GL_TEXTURE0);
	tex2d[0] = glIsEnabled(GL_TEXTURE_2D);
	glGetIntegerv(GL_TEXTURE_BINDING_2D, &bound);
	glGetTexEnviv(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, &envmode);

	if (!ready)
		setup();

	if (!failed) {
		glViewport(0, 0, st.w, st.h);
		glMatrixMode(GL_TEXTURE);
		glPushMatrix();
		glLoadIdentity();
		glMatrixMode(GL_PROJECTION);
		glPushMatrix();
		glLoadIdentity();
		glOrtho(0, st.w, st.h, 0, -1, 1);
		glMatrixMode(GL_MODELVIEW);
		glPushMatrix();
		glLoadIdentity();
		for (unsigned i = 0; i < sizeof caps / sizeof *caps; i++)
			if (caps[i] != GL_BLEND)
				glDisable(caps[i]);
		glEnable(GL_BLEND);
		glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
		glDepthMask(GL_FALSE);
		glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);

		if (st.osk)
			draw_keyboard(&st);
		if (st.cursor)
			draw_pointer(&st);

		glMatrixMode(GL_MODELVIEW);
		glPopMatrix();
		glMatrixMode(GL_PROJECTION);
		glPopMatrix();
		glMatrixMode(GL_TEXTURE);
		glPopMatrix();
	}

	glMatrixMode(mode);
	glViewport(vp[0], vp[1], vp[2], vp[3]);
	glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, envmode);
	glBindTexture(GL_TEXTURE_2D, bound);
	if (tex2d[0])
		glEnable(GL_TEXTURE_2D);
	else
		glDisable(GL_TEXTURE_2D);
	glActiveTexture(GL_TEXTURE1);
	if (tex2d[1])
		glEnable(GL_TEXTURE_2D);
	glActiveTexture(active);
	glClientActiveTexture(client);
	for (unsigned i = 0; i < sizeof caps / sizeof *caps; i++)
		if (was[i])
			glEnable(caps[i]);
		else
			glDisable(caps[i]);
	glBlendFunc(src, dst);
	glDepthMask(depthmask);
	glColor4f(color[0], color[1], color[2], color[3]);
}

/* ---- the hooks ---- */
typedef void *(*getproc_t)(const GLubyte *);

void glXSwapBuffers(void *dpy, unsigned long drawable)
{
	static void (*real)(void *, unsigned long);
	if (!real)
		real = (void (*)(void *, unsigned long))dlsym(RTLD_NEXT, "glXSwapBuffers");
	overlay();
	real(dpy, drawable);
}

static void *lookup(const char *which, const GLubyte *name)
{
	if (!strcmp((const char *)name, "glXSwapBuffers"))
		return (void *)glXSwapBuffers;
	getproc_t real = (getproc_t)dlsym(RTLD_NEXT, which);
	return real ? real(name) : NULL;
}

void *glXGetProcAddressARB(const GLubyte *name)
{
	return lookup("glXGetProcAddressARB", name);
}

void *glXGetProcAddress(const GLubyte *name)
{
	return lookup("glXGetProcAddress", name);
}
