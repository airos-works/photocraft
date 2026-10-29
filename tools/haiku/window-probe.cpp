/*
 * window-probe: lists a running PhotoCraft's windows and drives its pointer
 * the way the input server does, for testing the floating panels' own windows
 * (docs/haiku.md, Windows) on a machine where nobody holds the mouse.
 *
 *   window-probe TEAM list            each window: index, title, frame, look,
 *                                     feel, hidden, active
 *   window-probe TEAM drag X0 Y0 X1 Y1 [STEPS [MS]]
 *                                     presses the primary button at screen
 *                                     point (X0, Y0), moves to (X1, Y1) in
 *                                     STEPS moves (default 20) MS apart
 *                                     (default 16), and releases it
 *   window-probe TEAM click X Y       a click at screen point (X, Y)
 *   window-probe activate SIGNATURE   activates another application (the
 *                                     PhotoCraft copy loses focus)
 *   window-probe TEAM enter TITLE     Enter in the window TITLE of any team
 *                                     (its default button: a printer driver's
 *                                     setup dialog, whose application is busy
 *                                     running it and doesn't answer scripting)
 *
 * The messages go to the canvas view of the window under the starting point
 * (the smallest of the team's shown windows that contains it, or window
 * PROBE_WINDOW when that variable is set: it can't tell which is in front), with
 * "screen_where" as app_server sends them: BWindow works out the view's own
 * coordinates when it dispatches each one, so a window that moves under a
 * drag sees the pointer move as it would. The real pointer doesn't move.
 *
 * Build on air/OS: g++ -I/boot/system/develop/headers/private/app
 *   -I/boot/system/develop/headers/private/shared -o window-probe
 *   window-probe.cpp -lbe
 */

#include <Application.h>
#include <Message.h>
#include <Messenger.h>
#include <OS.h>
#include <Rect.h>
#include <Roster.h>
#include <String.h>
#include <View.h>

#include <private/app/MessengerPrivate.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>


static BMessenger sApp;


static status_t
Get(int32 window, const char* property, BMessage& reply)
{
	BMessage message(B_GET_PROPERTY);
	message.AddSpecifier(property);
	message.AddSpecifier("Window", window);
	reply.MakeEmpty();
	status_t status = sApp.SendMessage(&message, &reply, 1000000, 1000000);
	if (status != B_OK)
		return status;
	int32 error;
	if (reply.FindInt32("error", &error) == B_OK && error != B_OK)
		return error;
	return B_OK;
}


static int32
CountWindows()
{
	BMessage message(B_COUNT_PROPERTIES);
	message.AddSpecifier("Window");
	BMessage reply;
	int32 count = 0;
	if (sApp.SendMessage(&message, &reply, 1000000, 1000000) == B_OK)
		reply.FindInt32("result", &count);
	return count;
}


static bool
Frame(int32 window, BRect& frame, bool& hidden)
{
	BMessage reply;
	if (Get(window, "Frame", reply) != B_OK || reply.FindRect("result", &frame) != B_OK)
		return false;
	hidden = false;
	if (Get(window, "Hidden", reply) == B_OK)
		reply.FindBool("result", &hidden);
	return true;
}


// The canvas view of the smallest shown window containing the point.
static BMessenger
CanvasAt(BPoint where, int32* index)
{
	int32 best = -1;
	float bestArea = 0;
	const char* forced = getenv("PROBE_WINDOW");
	for (int32 i = 0; forced == NULL && i < CountWindows(); i++) {
		BRect frame;
		bool hidden;
		if (!Frame(i, frame, hidden) || hidden || !frame.Contains(where))
			continue;
		float area = frame.Width() * frame.Height();
		if (best < 0 || area < bestArea) {
			best = i;
			bestArea = area;
		}
	}
	if (forced != NULL)
		best = atol(forced);
	*index = best;
	if (best < 0)
		return BMessenger();
	BMessage message(B_GET_PROPERTY);
	message.AddSpecifier("Messenger");
	message.AddSpecifier("View", "canvas");
	message.AddSpecifier("Window", best);
	BMessage reply;
	BMessenger view;
	if (sApp.SendMessage(&message, &reply, 1000000, 1000000) == B_OK)
		reply.FindMessenger("result", &view);
	return view;
}


static void
Mouse(BMessenger& view, uint32 what, BPoint where, int32 buttons)
{
	BMessage message(what);
	message.AddInt64("when", system_time());
	message.AddPoint("screen_where", where);
	message.AddInt32("buttons", buttons);
	message.AddInt32("modifiers", 0);
	if (what == B_MOUSE_DOWN)
		message.AddInt32("clicks", 1);
	if (what == B_MOUSE_MOVED)
		message.AddInt32("be:transit", B_INSIDE_VIEW);
	status_t status = view.SendMessage(&message, (BHandler*)NULL, 1000000);
	if (status != B_OK)
		fprintf(stderr, "send: %s\n", strerror(status));
}


int
main(int argc, char** argv)
{
	BApplication app("application/x-vnd.airos-window-probe");
	if (argc >= 3 && strcmp(argv[1], "activate") == 0) {
		status_t status = be_roster->ActivateApp(be_roster->TeamFor(argv[2]));
		printf("activate %s: %s\n", argv[2], strerror(status));
		return status == B_OK ? 0 : 1;
	}
	if (argc < 3) {
		fprintf(stderr, "usage: see the comment at the top of window-probe.cpp\n");
		return 2;
	}
	sApp = BMessenger(NULL, (team_id)atol(argv[1]));
	if (!sApp.IsValid() && strcmp(argv[2], "enter") != 0) {
		fprintf(stderr, "no application with team %s\n", argv[1]);
		return 1;
	}
	if (strcmp(argv[2], "list") == 0) {
		for (int32 i = 0; i < CountWindows(); i++) {
			BRect frame;
			bool hidden;
			BMessage reply;
			const char* title = "";
			int32 look = -1, feel = -1;
			bool active = false;
			Frame(i, frame, hidden);
			if (Get(i, "Title", reply) == B_OK)
				reply.FindString("result", &title);
			BString name(title);
			if (Get(i, "Look", reply) == B_OK)
				reply.FindInt32("result", &look);
			if (Get(i, "Feel", reply) == B_OK)
				reply.FindInt32("result", &feel);
			if (Get(i, "Active", reply) == B_OK)
				reply.FindBool("result", &active);
			printf("%" B_PRId32 "\t%s\t%g %g %g %g\tlook %" B_PRId32
				"\tfeel %" B_PRId32 "%s%s\n", i, name.String(), frame.left,
				frame.top, frame.right, frame.bottom, look, feel,
				hidden ? "\thidden" : "", active ? "\tactive" : "");
		}
		return 0;
	}
	if (strcmp(argv[2], "enter") == 0 && argc >= 4) {
		// A window's looper port is named after its title.
		team_id team = (team_id)atol(argv[1]);
		port_info info;
		int32 cookie = 0;
		port_id port = -1;
		while (get_next_port_info(team, &cookie, &info) == B_OK) {
			if (strcmp(info.name, argv[3]) == 0)
				port = info.port;
		}
		if (port < 0) {
			fprintf(stderr, "no window %s\n", argv[3]);
			return 1;
		}
		BMessenger window;
		BMessenger::Private(window).SetTo(team, port, B_PREFERRED_TOKEN);
		BMessage key(B_KEY_DOWN);
		key.AddInt64("when", system_time());
		key.AddInt32("key", 0x47);
		key.AddInt32("raw_char", B_ENTER);
		key.AddInt32("modifiers", 0);
		key.AddString("bytes", "\n");
		key.AddInt8("byte", B_ENTER);
		status_t status = window.SendMessage(&key);
		printf("enter: %s\n", strerror(status));
		return status == B_OK ? 0 : 1;
	}
	if ((strcmp(argv[2], "drag") == 0 && argc >= 7)
		|| (strcmp(argv[2], "click") == 0 && argc >= 5)) {
		bool drag = strcmp(argv[2], "drag") == 0;
		BPoint from(atof(argv[3]), atof(argv[4]));
		BPoint to = drag ? BPoint(atof(argv[5]), atof(argv[6])) : from;
		int32 steps = drag && argc >= 8 ? atol(argv[7]) : 20;
		bigtime_t pause = (drag && argc >= 9 ? atol(argv[8]) : 16) * 1000;
		int32 index;
		BMessenger view = CanvasAt(from, &index);
		if (!view.IsValid()) {
			fprintf(stderr, "no window of the team at %g, %g\n", from.x, from.y);
			return 1;
		}
		printf("window %" B_PRId32 "\n", index);
		Mouse(view, B_MOUSE_MOVED, from, 0);
		snooze(pause);
		Mouse(view, B_MOUSE_DOWN, from, B_PRIMARY_MOUSE_BUTTON);
		snooze(pause * 3);
		if (drag) {
			for (int32 i = 1; i <= steps; i++) {
				BPoint at = from + BPoint((to.x - from.x) * i / steps,
					(to.y - from.y) * i / steps);
				Mouse(view, B_MOUSE_MOVED, at, B_PRIMARY_MOUSE_BUTTON);
				snooze(pause);
			}
		}
		snooze(pause * 3);
		Mouse(view, B_MOUSE_UP, to, 0);
		return 0;
	}
	fprintf(stderr, "usage: see the comment at the top of window-probe.cpp\n");
	return 2;
}
