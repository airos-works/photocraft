// Pen tablet fixture for PhotoCraft's native host (no tablet needed): sends the
// canvas view a stroke as Haiku's tablet input add-ons would, B_MOUSE_DOWN,
// B_MOUSE_MOVED and B_MOUSE_UP with be:tablet_pressure, be:tablet_tilt_x/y and
// be:tablet_eraser (src/add-ons/input_server/devices/tablet). Pressure ramps
// from 0.05 to 1 along the stroke, so the brush's dabs grow from thin to full.
//
//   tablet-probe X0 Y0 X1 Y1 [STEPS] [hid|wacom] [eraser]
//
// Points are in the canvas view's coordinates. "hid" (the default) sends what
// the HID tablet add-on sends: no sample with the button messages, tilt 0..1;
// "wacom" sends a sample with every message, tilt -1..1, and no subtype.
// Build: x86_64-unknown-haiku-g++ tablet-probe.cpp -lbe -o tablet-probe
#include <Application.h>
#include <Input.h>
#include <InterfaceDefs.h>
#include <Message.h>
#include <Messenger.h>
#include <OS.h>
#include <Rect.h>
#include <View.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>


static const char* kSignature = "application/x-vnd.storytold-PhotoCraft";


static bool
Property(const char* property, bool view, BMessage& reply)
{
	BMessenger target(kSignature);
	BMessage request(B_GET_PROPERTY);
	request.AddSpecifier(property);
	if (view)
		request.AddSpecifier("View", "canvas");
	request.AddSpecifier("Window", (int32)0);
	return target.SendMessage(&request, &reply, 1000000, 1000000) == B_OK;
}


int
main(int argc, char** argv)
{
	if (argc < 5) {
		fprintf(stderr, "usage: %s X0 Y0 X1 Y1 [STEPS] [hid|wacom] [eraser]\n",
			argv[0]);
		return 2;
	}
	BApplication app("application/x-vnd.PhotoCraft-TabletTest");
	float x0 = atof(argv[1]), y0 = atof(argv[2]);
	float x1 = atof(argv[3]), y1 = atof(argv[4]);
	int steps = argc > 5 ? atoi(argv[5]) : 40;
	bool wacom = argc > 6 && strcmp(argv[6], "wacom") == 0;
	bool eraser = argc > 7 && strcmp(argv[7], "eraser") == 0;
	if (steps < 2)
		steps = 2;

	BMessage reply;
	BMessenger view;
	BRect windowFrame, viewFrame;
	if (!Property("Messenger", true, reply)
		|| reply.FindMessenger("result", &view) != B_OK)
		return 3;
	reply.MakeEmpty();
	if (!Property("Frame", false, reply)
		|| reply.FindRect("result", &windowFrame) != B_OK)
		return 4;
	reply.MakeEmpty();
	if (!Property("Frame", true, reply)
		|| reply.FindRect("result", &viewFrame) != B_OK)
		return 5;
	BPoint origin = windowFrame.LeftTop() + viewFrame.LeftTop();

	auto send = [&](uint32 what, float x, float y, int32 buttons,
		float pressure, float tilt, bool sample) {
		BMessage message(what);
		message.AddInt64("when", system_time());
		message.AddPoint("screen_where", origin + BPoint(x, y));
		message.AddInt32("buttons", buttons);
		message.AddInt32("modifiers", 0);
		if (what == B_MOUSE_DOWN)
			message.AddInt32("clicks", 1);
		if (!wacom)
			message.AddInt32("be:device_subtype", B_TABLET_POINTING_DEVICE);
		if (sample) {
			message.AddFloat("be:tablet_x", x);
			message.AddFloat("be:tablet_y", y);
			message.AddFloat("be:tablet_pressure", pressure);
			message.AddInt32("be:tablet_eraser", eraser ? 1 : 0);
			// Tilted to the right: 32 degrees either way.
			message.AddFloat("be:tablet_tilt_x", wacom ? tilt : 0.5f + tilt / 2);
			message.AddFloat("be:tablet_tilt_y", wacom ? 0.0f : 0.5f);
		}
		status_t status = view.SendMessage(&message);
		snooze(8000);
		return status;
	};

	// The pen comes into range, hovering at pressure 0, then touches.
	send(B_MOUSE_MOVED, x0, y0, 0, 0, 0.5f, true);
	status_t status = send(B_MOUSE_DOWN, x0, y0, B_PRIMARY_MOUSE_BUTTON, 0.05f,
		0.5f, wacom);
	for (int i = 0; i <= steps; i++) {
		float t = (float)i / steps;
		send(B_MOUSE_MOVED, x0 + (x1 - x0) * t, y0 + (y1 - y0) * t,
			B_PRIMARY_MOUSE_BUTTON, 0.05f + 0.95f * t, 0.5f, true);
	}
	send(B_MOUSE_UP, x1, y1, 0, 0, 0.5f, wacom);
	send(B_MOUSE_MOVED, x1, y1 + 1, 0, 0, 0.5f, true);
	printf("tablet stroke sent (%s%s, %d steps): %s\n", wacom ? "wacom" : "hid",
		eraser ? ", eraser" : "", steps, strerror(status));
	return status == B_OK ? 0 : 6;
}
