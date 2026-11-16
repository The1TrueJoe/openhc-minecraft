/* capture.c: the video half of mc-record; call before each real glXSwapBuffers. */
void capture_frame(void *dpy, unsigned long drawable);
