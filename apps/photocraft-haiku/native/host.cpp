/*
 * PhotoCraft's air/OS (Haiku) host: the application, its windows (one per egui
 * viewport: the main window and the floating panels' and documents' own) and
 * the system services the Rust side reaches through the C functions at the end
 * of this file (bound in src/host.rs).
 *
 * Threads: be_app runs its message loop on the process's main thread
 * (pc_host_run), each window on its own looper thread, and the Rust interface
 * on a thread of its own. Input arrives in the window threads and is queued
 * here, tagged with its window's id, for the Rust thread, which waits on the
 * queue between frames (pc_host_wait) and hands back finished frames as 32-bit
 * BGRA pixels (pc_host_present), or puts them straight into the screen itself
 * (the windows are direct windows; src/present.rs). Every call from the Rust
 * thread that touches a window locks it first.
 */

#include <Alert.h>
#include <Application.h>
#include <Beep.h>
#include <Bitmap.h>
#include <BitmapStream.h>
#include <Clipboard.h>
#include <Cursor.h>
#include <DataIO.h>
#include <DirectWindow.h>
#include <Directory.h>
#include <Entry.h>
#include <FilePanel.h>
#include <FindDirectory.h>
#include <Font.h>
#include <FormattingConventions.h>
#include <Input.h>
#include <InterfaceDefs.h>
#include <Locale.h>
#include <LocaleRoster.h>
#include <Menu.h>
#include <MenuBar.h>
#include <MenuItem.h>
#include <Message.h>
#include <Messenger.h>
#include <Mime.h>
#include <Node.h>
#include <Path.h>
#include <Region.h>
#include <Roster.h>
#include <Screen.h>
#include <String.h>
#include <TranslationUtils.h>
#include <TranslatorFormats.h>
#include <TranslatorRoster.h>
#include <Url.h>
#include <View.h>
#include <Window.h>

#include <dlfcn.h>
#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <deque>
#include <map>
#include <mutex>
#include <string>
#include <vector>


extern "C" {

// Keep in step with `Event` in src/host.rs.
struct PcEvent {
	int32_t kind;
	// The window it happened in (pc_host_open_window's id); 0 for the
	// application's own events.
	int32_t window;
	int32_t buttons;
	int32_t clicks;
	uint32_t modifiers;
	int32_t key;
	int32_t raw_char;
	int32_t repeat;
	int32_t id;
	float x;
	float y;
	float dx;
	float dy;
	uint32_t text_len;
	// What a pen tablet's input add-on reported with a mouse event (see
	// ReadTablet): tablet 1 with a sample, 2 for a tablet's message without
	// one, 0 for anything else. Tilt as the add-on sent it (has_tilt).
	int32_t tablet;
	float pressure;
	float tilt_x;
	float tilt_y;
	int32_t has_tilt;
	int32_t eraser;
	// be:device_subtype (B_*_POINTING_DEVICE), -1 when the message has none.
	int32_t subtype;
};

struct PcWindowInfo {
	float inner_left, inner_top, inner_right, inner_bottom;
	float outer_left, outer_top, outer_right, outer_bottom;
	// The monitor most of the window is on: its frame, edges excluded.
	float monitor_left, monitor_top, monitor_right, monitor_bottom;
	int32_t minimized;
	int32_t active;
	int32_t zoomed;
	int32_t fullscreen;
	int32_t hidden;
};

}


enum {
	PC_MOUSE_MOVED = 1,
	PC_MOUSE_DOWN = 2,
	PC_MOUSE_UP = 3,
	PC_WHEEL = 4,
	PC_KEY_DOWN = 5,
	PC_KEY_UP = 6,
	PC_RESIZED = 7,
	PC_FOCUS = 8,
	PC_CLOSE = 9,
	PC_OPEN_FILES = 10,
	PC_DROPPED_FILES = 11,
	PC_POINTER_GONE = 12,
	PC_QUIT_REQUESTED = 13,
	PC_DIALOG_ANSWER = 14,
	PC_MODIFIERS = 15,
	PC_MOVED = 16,
	PC_MINIMIZED = 17,
	PC_COLORS = 18,
	PC_MENU = 19,
	PC_SCREEN_PICK = 20,
};

static const uint32 kDialogAnswer = 'dcDA';
static const char* kDialogField = "dc:dialog";

// The native menu bar's messages (pc_menu_build): an item of the interface's
// (its index in "index"), and the window's own Minimize, Zoom and Close.
static const uint32 kMenuItem = 'pcMI';
static const uint32 kMenuMinimize = 'pcMm';
static const uint32 kMenuZoom = 'pcMz';
static const uint32 kMenuClose = 'pcMc';


// #pragma mark - Event queue


struct QueuedEvent {
	PcEvent event;
	std::string text;
};

static std::mutex sQueueLock;
static std::condition_variable sQueueCondition;
static std::deque<QueuedEvent> sQueue;
static bool sWoken = false;
// The text of the event pc_host_next_event returned last.
static std::string sCurrentText;
static volatile bool sAllowQuit = false;


static PcEvent
MakeEvent(int32_t kind, int32_t window = 0)
{
	PcEvent event;
	memset(&event, 0, sizeof(event));
	event.kind = kind;
	event.window = window;
	event.modifiers = modifiers();
	event.subtype = -1;
	return event;
}


static void
Push(const PcEvent& event, const std::string& text = std::string())
{
	{
		std::lock_guard<std::mutex> lock(sQueueLock);
		// A burst of moves only needs the last position; a pen's every
		// sample counts (its pressure along the stroke).
		if (event.kind == PC_MOUSE_MOVED && event.tablet != 1 && !sQueue.empty()
			&& sQueue.back().event.kind == PC_MOUSE_MOVED
			&& sQueue.back().event.window == event.window
			&& sQueue.back().event.tablet != 1
			&& sQueue.back().event.buttons == event.buttons) {
			sQueue.back().event = event;
		} else {
			QueuedEvent queued;
			queued.event = event;
			queued.text = text;
			sQueue.push_back(queued);
		}
	}
	sQueueCondition.notify_all();
}


// The paths of a message's refs, one per line. As documents, without the
// application's own executable: a launch through a link to it can hand it its
// own ref, and that is never a PDF.
static std::string
PathsOf(const BMessage* message, bool documents = false)
{
	app_info self;
	bool knowSelf = documents && be_app->GetAppInfo(&self) == B_OK;
	std::string paths;
	entry_ref ref;
	for (int32 i = 0; message->FindRef("refs", i, &ref) == B_OK; i++) {
		BEntry entry(&ref, true);
		entry_ref resolved;
		BPath path;
		if (entry.GetRef(&resolved) != B_OK || entry.GetPath(&path) != B_OK)
			continue;
		if (knowSelf && resolved == self.ref)
			continue;
		if (!paths.empty())
			paths += '\n';
		paths += path.Path();
	}
	return paths;
}


// #pragma mark - Direct connection


#ifndef B_DIRECT_DEVICE_PIXELS
#define B_DIRECT_DEVICE_PIXELS 0x00000400
#endif

// Keep in step with `RawDirect` in src/host.rs.
typedef struct {
	int32_t connected;
	int32_t bounds[4];
	uint32_t bytesPerRow;
	uint32_t rectCount;
	const int32_t* rects;
} PcDirect;

// Called with the window's id and its direct connection (src/host.rs).
typedef void (*PcDirectSink)(int32_t window, const PcDirect* direct);

static const uint32 kMaxDirectRects = 256;


// Frames go straight into the screen only when PHOTOCRAFT_PRESENT=direct asks for
// it (src/present.rs) and the GPU draws them.
static uint32
DirectPresentFlags()
{
	const char* present = getenv("PHOTOCRAFT_PRESENT");
	const char* renderer = getenv("PHOTOCRAFT_RENDERER");
	if (present == NULL || strcmp(present, "direct") != 0
		|| (renderer != NULL && strcmp(renderer, "cpu") == 0))
		return 0;
	return B_DIRECT_DEVICE_PIXELS;
}


// #pragma mark - Pen tablets


// What a pen tablet sent with a mouse message. Haiku's tablet input add-ons
// (input_server/devices/tablet for HID tablets, wacom, easypen) put the pen's
// state in B_MOUSE_MOVED: be:tablet_pressure (0..1), be:tablet_eraser, and
// be:tablet_tilt_x/y when the pen reports tilt. Their B_MOUSE_DOWN and
// B_MOUSE_UP (except wacom's) carry only be:device_subtype
// B_TABLET_POINTING_DEVICE: the pen is still down, at its last pressure.
// The tilt is passed on as sent; src/tablet.rs reads its units.
static void
ReadTablet(const BMessage* message, PcEvent& event)
{
	if (message == NULL)
		return;
	int32 subtype;
	if (message->FindInt32("be:device_subtype", &subtype) == B_OK)
		event.subtype = subtype;
	float pressure;
	if (message->FindFloat("be:tablet_pressure", &pressure) == B_OK) {
		event.tablet = 1;
		event.pressure = pressure;
		int32 eraser = 0;
		message->FindInt32("be:tablet_eraser", &eraser);
		event.eraser = eraser != 0;
		if (message->FindFloat("be:tablet_tilt_x", &event.tilt_x) == B_OK
			&& message->FindFloat("be:tablet_tilt_y", &event.tilt_y) == B_OK)
			event.has_tilt = 1;
	} else if (event.subtype == B_TABLET_POINTING_DEVICE)
		event.tablet = 2;
}


// #pragma mark - Canvas view


// Edges a window resize drag moves (pc_host_window_command 13); none moves
// the whole window (12).
enum {
	PC_EDGE_LEFT = 1,
	PC_EDGE_RIGHT = 2,
	PC_EDGE_TOP = 4,
	PC_EDGE_BOTTOM = 8,
};


class PcView : public BView {
public:
	PcView(BRect frame, int32 id)
		:
		BView(frame, "canvas", B_FOLLOW_ALL_SIDES, B_WILL_DRAW | B_FRAME_EVENTS),
		fId(id),
		fBitmap(NULL),
		fScale(1),
		fCursor(-1),
		fDragging(false),
		fDragEdges(0)
	{
		SetViewColor(B_TRANSPARENT_COLOR);
		SetDrawingMode(B_OP_COPY);
	}

	~PcView()
	{
		delete fBitmap;
	}

	virtual void Draw(BRect updateRect)
	{
		// The frame is in device pixels (fScale per logical pixel, on an
		// air/OS desktop drawn at a higher density): app_server scales the
		// logical destination back up, so it lands one to one.
		BRect painted;
		if (fBitmap != NULL) {
			BRect logical = fBitmap->Bounds();
			logical.right = (logical.right + 1) / fScale - 1;
			logical.bottom = (logical.bottom + 1) / fScale - 1;
			painted = logical & updateRect;
			if (painted.IsValid()) {
				BRect source(painted.left * fScale, painted.top * fScale,
					(painted.right + 1) * fScale - 1,
					(painted.bottom + 1) * fScale - 1);
				source = source & fBitmap->Bounds();
				DrawBitmapAsync(fBitmap, source, painted);
			}
		}
		// Uncovered parts (the window grew and the next frame isn't there
		// yet) get the panel colour instead of garbage.
		SetHighColor(ui_color(B_PANEL_BACKGROUND_COLOR));
		BRegion rest(updateRect);
		if (painted.IsValid())
			rest.Exclude(painted);
		FillRegion(&rest);
	}

	virtual void MouseDown(BPoint where)
	{
		MakeFocus(true);
		PcEvent event = MakeEvent(PC_MOUSE_DOWN, fId);
		event.x = where.x;
		event.y = where.y;
		BMessage* message = Window()->CurrentMessage();
		if (message != NULL) {
			message->FindInt32("buttons", &event.buttons);
			message->FindInt32("clicks", &event.clicks);
		}
		ReadTablet(message, event);
		Track(message, where, event.buttons);
		// A mouse drag needs only the latest position; a pen's stroke wants
		// every sample, each with its pressure.
		SetMouseEventMask(B_POINTER_EVENTS,
			event.tablet != 0 ? 0 : B_NO_POINTER_HISTORY);
		Push(event);
	}

	virtual void MouseUp(BPoint where)
	{
		fDragging = false;
		PcEvent event = MakeEvent(PC_MOUSE_UP, fId);
		event.x = where.x;
		event.y = where.y;
		BMessage* message = Window()->CurrentMessage();
		if (message != NULL)
			message->FindInt32("buttons", &event.buttons);
		Track(message, where, event.buttons);
		ReadTablet(message, event);
		Push(event);
	}

	virtual void MouseMoved(BPoint where, uint32 transit,
		const BMessage* dragMessage)
	{
		PcEvent event = MakeEvent(PC_MOUSE_MOVED, fId);
		event.x = where.x;
		event.y = where.y;
		BMessage* message = Window()->CurrentMessage();
		if (message != NULL)
			message->FindInt32("buttons", &event.buttons);
		Track(message, where, event.buttons);
		if (fDragging) {
			// The interface asked to move or resize the window by this
			// press (an undecorated window's own title bar or edge): the
			// pointer moves the window, and the interface sees none of it.
			if (event.buttons != 0) {
				DragTo(fScreenWhere);
				return;
			}
			fDragging = false;
		}
		ReadTablet(message, event);
		if (transit == B_EXITED_VIEW && event.buttons == 0)
			event.kind = PC_POINTER_GONE;
		Push(event);
	}

	// Where the pointer is on the screen and which buttons are down, as the
	// last mouse message said.
	void Track(const BMessage* message, BPoint where, int32 buttons)
	{
		if (message == NULL
			|| message->FindPoint("screen_where", &fScreenWhere) != B_OK)
			fScreenWhere = ConvertToScreen(where);
		fButtons = buttons;
	}

	// Called with the window locked: the pointer, while its button is down,
	// moves the window (edges 0) or the window's edges (PC_EDGE_*). The
	// interface asks for it while it handles the press, after the window
	// got it: by the mouse messages since, not the pointer's state now.
	void StartDrag(int32 edges)
	{
		if (fButtons == 0)
			return;
		fDragging = true;
		fDragEdges = edges;
		fDragStart = fScreenWhere;
		fDragFrame = Window()->Frame();
	}

	void DragTo(BPoint screen)
	{
		BPoint delta = screen - fDragStart;
		BRect frame = fDragFrame;
		if (fDragEdges == 0)
			frame.OffsetBy(delta);
		else {
			float minWidth, maxWidth, minHeight, maxHeight;
			Window()->GetSizeLimits(&minWidth, &maxWidth, &minHeight,
				&maxHeight);
			if ((fDragEdges & PC_EDGE_LEFT) != 0) {
				frame.left = std::max(std::min(frame.left + delta.x,
					frame.right - minWidth), frame.right - maxWidth);
			}
			if ((fDragEdges & PC_EDGE_RIGHT) != 0) {
				frame.right = std::min(std::max(frame.right + delta.x,
					frame.left + minWidth), frame.left + maxWidth);
			}
			if ((fDragEdges & PC_EDGE_TOP) != 0) {
				frame.top = std::max(std::min(frame.top + delta.y,
					frame.bottom - minHeight), frame.bottom - maxHeight);
			}
			if ((fDragEdges & PC_EDGE_BOTTOM) != 0) {
				frame.bottom = std::min(std::max(frame.bottom + delta.y,
					frame.top + minHeight), frame.top + maxHeight);
			}
		}
		Window()->MoveTo(roundf(frame.left), roundf(frame.top));
		if (fDragEdges != 0)
			Window()->ResizeTo(roundf(frame.Width()), roundf(frame.Height()));
	}

	// Called with the window locked: one of Haiku's BCursorIDs.
	void SetCursorId(int32 id)
	{
		static BCursor* sCursors[32] = {};
		if (id < 1 || id > 31)
			id = B_CURSOR_ID_SYSTEM_DEFAULT;
		if (id == fCursor)
			return;
		if (sCursors[id] == NULL)
			sCursors[id] = new BCursor((BCursorID)id);
		SetViewCursor(sCursors[id]);
		fCursor = id;
	}

	virtual void KeyDown(const char* bytes, int32 numBytes)
	{
		Key(PC_KEY_DOWN, Window()->CurrentMessage(), bytes, numBytes);
	}

	virtual void KeyUp(const char* bytes, int32 numBytes)
	{
		Key(PC_KEY_UP, Window()->CurrentMessage(), bytes, numBytes);
	}

	void Key(int32 kind, BMessage* message, const char* bytes, int32 numBytes)
	{
		PcEvent event = MakeEvent(kind, fId);
		if (message != NULL) {
			message->FindInt32("key", &event.key);
			message->FindInt32("raw_char", &event.raw_char);
			int32 modifiers;
			if (message->FindInt32("modifiers", &modifiers) == B_OK)
				event.modifiers = modifiers;
			int32 repeat;
			if (message->FindInt32("be:key_repeat", &repeat) == B_OK)
				event.repeat = repeat;
		}
		std::string text(bytes != NULL ? bytes : "",
			bytes != NULL && numBytes > 0 ? numBytes : 0);
		event.text_len = text.size();
		Push(event, text);
	}

	virtual void FrameResized(float width, float height)
	{
		PcEvent event = MakeEvent(PC_RESIZED, fId);
		event.x = width + 1;
		event.y = height + 1;
		Push(event);
	}

	virtual void MessageReceived(BMessage* message)
	{
		if (message->WasDropped() && message->HasRef("refs")) {
			BPoint where = ConvertFromScreen(message->DropPoint());
			PcEvent event = MakeEvent(PC_DROPPED_FILES, fId);
			event.x = where.x;
			event.y = where.y;
			std::string paths = PathsOf(message, true);
			event.text_len = paths.size();
			Push(event, paths);
			return;
		}
		switch (message->what) {
			case B_MOUSE_WHEEL_CHANGED:
			{
				// A wheel's whole steps, or a touchpad's scroll gesture
				// (air/OS): its unrounded steps, and the gesture's phase in
				// clicks (1 began, 2 changed, 3 ended), repeat 1 for the
				// precise deltas.
				PcEvent event = MakeEvent(PC_WHEEL, fId);
				message->FindInt32("be:device_subtype", &event.subtype);
				float dx, dy;
				int32 phase;
				if (message->FindInt32("be:wheel_phase", &phase) == B_OK
					&& message->FindFloat("be:wheel_precise_delta_x", &dx) == B_OK
					&& message->FindFloat("be:wheel_precise_delta_y", &dy) == B_OK) {
					event.dx = dx;
					event.dy = dy;
					event.clicks = phase;
					event.repeat = 1;
				} else {
					message->FindFloat("be:wheel_delta_x", &event.dx);
					message->FindFloat("be:wheel_delta_y", &event.dy);
				}
				Push(event);
				break;
			}
			case B_MODIFIERS_CHANGED:
				Push(MakeEvent(PC_MODIFIERS, fId));
				break;
			default:
				BView::MessageReceived(message);
		}
	}

	// Called with the window locked. The pixels are device pixels, scale
	// per logical pixel.
	void Present(const uint32_t* pixels, int32 width, int32 height,
		int32 x0, int32 y0, int32 x1, int32 y1, float scale)
	{
		if (scale != fScale) {
			fScale = scale;
			delete fBitmap;
			fBitmap = NULL;
		}
		if (fBitmap == NULL || fBitmap->Bounds().IntegerWidth() + 1 != width
			|| fBitmap->Bounds().IntegerHeight() + 1 != height) {
			delete fBitmap;
			fBitmap = new BBitmap(BRect(0, 0, width - 1, height - 1), B_RGB32);
			if (fBitmap->InitCheck() != B_OK) {
				delete fBitmap;
				fBitmap = NULL;
				return;
			}
			x0 = 0;
			y0 = 0;
			x1 = width;
			y1 = height;
		}
		if (x0 < 0)
			x0 = 0;
		if (y0 < 0)
			y0 = 0;
		if (x1 > width)
			x1 = width;
		if (y1 > height)
			y1 = height;
		if (x1 <= x0 || y1 <= y0)
			return;
		uint8* bits = (uint8*)fBitmap->Bits();
		int32 bytesPerRow = fBitmap->BytesPerRow();
		for (int32 y = y0; y < y1; y++) {
			memcpy(bits + (size_t)y * bytesPerRow + (size_t)x0 * 4,
				pixels + (size_t)y * width + x0, (size_t)(x1 - x0) * 4);
		}
		Invalidate(BRect(floorf(x0 / fScale), floorf(y0 / fScale),
			ceilf(x1 / fScale) - 1, ceilf(y1 / fScale) - 1));
	}

	// Called with the window locked: the rectangles ([x0, y0, x1, y1) device
	// pixels, four values each) of a frame read back from the GPU. While the
	// window puts frames straight into the screen they are drawn at once and
	// waited for: an update drawn later could put this frame over a newer one
	// the GPU has put there in between.
	void PresentRects(const uint32_t* pixels, int32 width, int32 height,
		const int32_t* rects, int32 count, float scale, bool direct)
	{
		const int32_t all[4] = { 0, 0, width, height };
		if (scale != fScale || fBitmap == NULL
			|| fBitmap->Bounds().IntegerWidth() + 1 != width
			|| fBitmap->Bounds().IntegerHeight() + 1 != height) {
			// A new bitmap: all of it.
			fScale = scale;
			delete fBitmap;
			fBitmap = new BBitmap(BRect(0, 0, width - 1, height - 1), B_RGB32);
			if (fBitmap->InitCheck() != B_OK) {
				delete fBitmap;
				fBitmap = NULL;
				return;
			}
			rects = all;
			count = 1;
		}
		uint8* bits = (uint8*)fBitmap->Bits();
		int32 bytesPerRow = fBitmap->BytesPerRow();
		for (int32 i = 0; i < count; i++) {
			int32 x0 = std::max(rects[i * 4], (int32_t)0);
			int32 y0 = std::max(rects[i * 4 + 1], (int32_t)0);
			int32 x1 = std::min(rects[i * 4 + 2], width);
			int32 y1 = std::min(rects[i * 4 + 3], height);
			if (x1 <= x0 || y1 <= y0)
				continue;
			for (int32 y = y0; y < y1; y++) {
				memcpy(bits + (size_t)y * bytesPerRow + (size_t)x0 * 4,
					pixels + (size_t)y * width + x0, (size_t)(x1 - x0) * 4);
			}
			BRect logical(floorf(x0 / fScale), floorf(y0 / fScale),
				ceilf(x1 / fScale) - 1, ceilf(y1 / fScale) - 1);
			if (direct) {
				BRect source(logical.left * fScale, logical.top * fScale,
					(logical.right + 1) * fScale - 1,
					(logical.bottom + 1) * fScale - 1);
				DrawBitmapAsync(fBitmap, source & fBitmap->Bounds(), logical);
			} else
				Invalidate(logical);
		}
		if (direct)
			Sync();
	}

private:
	int32 fId;
	BBitmap* fBitmap;
	float fScale;
	int32 fCursor;
	bool fDragging;
	int32 fDragEdges;
	BPoint fDragStart;
	BRect fDragFrame;
	BPoint fScreenWhere;
	int32 fButtons = 0;
};


// #pragma mark - Window


// The frame of the monitor most of the window is on. On air/OS one desktop
// spans every monitor (BScreen's frame is all of them); its display layout
// knows each one. Elsewhere, the screen.
static BRect
MonitorFrame(BWindow* window)
{
	typedef status_t (*GetDisplayFrame)(BRect, bool, BRect&);
	static GetDisplayFrame getFrame = (GetDisplayFrame)dlsym(RTLD_DEFAULT,
		"_ZN8BPrivate17get_display_frameE5BRectbRS0_");
	BRect display;
	if (getFrame != NULL && getFrame(window->Frame(), false, display) == B_OK
		&& display.IsValid())
		return display;
	return BScreen(window).Frame();
}


class PcWindow : public BDirectWindow {
public:
	PcWindow(int32 id, BRect frame, const char* title, window_look look,
		window_feel feel, uint32 flags)
		:
		BDirectWindow(frame, title, look, feel,
			B_ASYNCHRONOUS_CONTROLS | DirectPresentFlags() | flags),
		fId(id),
		fViewTop(0),
		fDirectSink(NULL),
		fDirectReported(false),
		fFullscreen(false),
		fSavedLook(look)
	{
		memset(&fDirect, 0, sizeof(fDirect));
		fView = new PcView(Bounds(), id);
		AddChild(fView);
		fView->MakeFocus(true);
		// The interface has its own Cut, Copy, Paste, Select All and Close
		// (with a Save Changes prompt), so these keys reach it as keys.
		RemoveShortcut('W', B_COMMAND_KEY);
		RemoveShortcut('X', B_COMMAND_KEY);
		RemoveShortcut('C', B_COMMAND_KEY);
		RemoveShortcut('V', B_COMMAND_KEY);
		RemoveShortcut('A', B_COMMAND_KEY);
	}

	virtual bool QuitRequested()
	{
		if (sAllowQuit)
			return true;
		Push(MakeEvent(PC_CLOSE, fId));
		return false;
	}

	virtual void DispatchMessage(BMessage* message, BHandler* handler)
	{
		// BWindow eats every Command+key it has no shortcut for; the
		// interface's shortcuts (Command+B for bold…) need them. Command+Q
		// still quits the Haiku way.
		if (message->what == B_KEY_DOWN) {
			int32 modifiers = 0;
			int32 rawChar = 0;
			const char* bytes = NULL;
			message->FindInt32("modifiers", &modifiers);
			message->FindInt32("raw_char", &rawChar);
			message->FindString("bytes", &bytes);
			if ((modifiers & B_COMMAND_KEY) != 0 && rawChar != 'q'
				&& rawChar != 'Q') {
				fView->Key(PC_KEY_DOWN, message, bytes,
					bytes != NULL ? strlen(bytes) : 0);
				return;
			}
		}
		BWindow::DispatchMessage(message, handler);
	}

	virtual void WindowActivated(bool active)
	{
		PcEvent event = MakeEvent(PC_FOCUS, fId);
		event.x = active ? 1 : 0;
		Push(event);
	}

	virtual void FrameMoved(BPoint where)
	{
		PcEvent event = MakeEvent(PC_MOVED, fId);
		event.x = where.x;
		event.y = where.y;
		Push(event);
	}

	virtual void Minimize(bool minimize)
	{
		BWindow::Minimize(minimize);
		PcEvent event = MakeEvent(PC_MINIMIZED, fId);
		event.x = minimize ? 1 : 0;
		Push(event);
	}

	virtual void ScreenChanged(BRect frame, color_space mode)
	{
		Push(MakeEvent(PC_COLORS, fId));
	}

	virtual void MessageReceived(BMessage* message)
	{
		switch (message->what) {
			case B_COLORS_UPDATED:
				Push(MakeEvent(PC_COLORS, fId));
				break;
			case kMenuItem:
			{
				PcEvent event = MakeEvent(PC_MENU, fId);
				if (message->FindInt32("index", &event.id) == B_OK)
					Push(event);
				break;
			}
			case kMenuMinimize:
				Minimize(true);
				break;
			case kMenuZoom:
				Zoom();
				break;
			case kMenuClose:
				// The interface's close, with its Save Changes prompt.
				PostMessage(B_QUIT_REQUESTED);
				break;
			default:
				BWindow::MessageReceived(message);
		}
	}

	PcView* View() const { return fView; }
	BMenuBar* MenuBar() const { return fMenuBar; }

	// Where the canvas view starts (below the menu bar), logical pixels.
	int32 ViewTop() const { return fViewTop; }

	// Puts the menu bar at the top (NULL: none) and the canvas below it.
	// Called with the window locked.
	void SetMenuBar(BMenuBar* bar)
	{
		int32 oldTop = fViewTop;
		if (bar != fMenuBar) {
			if (fMenuBar != NULL) {
				RemoveChild(fMenuBar);
				delete fMenuBar;
			}
			fMenuBar = bar;
			if (bar != NULL) {
				AddChild(bar);
				SetKeyMenuBar(bar);
			}
		}
		LayoutCanvas();
		// The window grows by the menu bar (or shrinks when it goes), so the
		// canvas keeps its size and minimum.
		int32 delta = fViewTop - oldTop;
		if (delta != 0 && !fFullscreen) {
			float minWidth, maxWidth, minHeight, maxHeight;
			GetSizeLimits(&minWidth, &maxWidth, &minHeight, &maxHeight);
			SetSizeLimits(minWidth, maxWidth, minHeight + delta,
				maxHeight + delta);
			ResizeBy(0, delta);
		}
	}

	void LayoutCanvas()
	{
		float top = 0;
		if (fMenuBar != NULL) {
			float width, height;
			fMenuBar->GetPreferredSize(&width, &height);
			fMenuBar->ResizeTo(Bounds().Width(), height);
			top = fMenuBar->Frame().bottom + 1;
		}
		BRect bounds = Bounds();
		fView->MoveTo(0, top);
		fView->ResizeTo(bounds.Width(), std::max(bounds.Height() - top, 0.0f));
		fViewTop = (int32_t)top;
	}

	// BDirectWindow's Zoom(BPoint, float, float) hides BWindow's Zoom().
	using BWindow::Zoom;

	// The direct daemon thread, without the window lock: app_server waits
	// here (half a second at most) before it moves, covers or hides the
	// window, and the presenter returns once the GPU has finished drawing
	// where the window was. With B_DIRECT_DEVICE_PIXELS (air/OS) the bounds
	// and clipping are frame buffer pixels.
	virtual void DirectConnected(direct_buffer_info* info)
	{
		std::lock_guard<std::mutex> lock(fDirectLock);
		bool usable = (info->buffer_state & B_DIRECT_MODE_MASK)
				!= B_DIRECT_STOP
			&& info->device_scale >= 100 && info->bits_per_pixel == 32
			&& (info->pixel_format == B_RGB32
				|| info->pixel_format == B_RGBA32)
			&& info->layout == B_BUFFER_NONINTERLEAVED
			&& info->orientation == B_BUFFER_TOP_TO_BOTTOM
			&& info->clip_list_count <= kMaxDirectRects;
		fDirect.connected = usable ? 1 : 0;
		fDirect.rects = fDirectRects;
		fDirect.rectCount = 0;
		if (usable) {
			// The canvas is the window below its menu bar.
			fDirect.bounds[0] = info->window_bounds.left;
			fDirect.bounds[1] = info->window_bounds.top
				+ fViewTop * info->device_scale / 100;
			fDirect.bounds[2] = info->window_bounds.right;
			fDirect.bounds[3] = info->window_bounds.bottom;
			fDirect.bytesPerRow = info->bytes_per_row;
			fDirect.rectCount = info->clip_list_count;
			for (uint32 i = 0; i < info->clip_list_count; i++) {
				fDirectRects[i * 4] = info->clip_list[i].left;
				fDirectRects[i * 4 + 1] = info->clip_list[i].top;
				fDirectRects[i * 4 + 2] = info->clip_list[i].right;
				fDirectRects[i * 4 + 3] = info->clip_list[i].bottom;
			}
		}
		fDirectReported = true;
		if (fDirectSink != NULL)
			fDirectSink(fId, &fDirect);
	}

	// Where the window's direct connection is reported (NULL: nowhere). The
	// last one is reported at once; unsetting waits for a report in
	// progress, as the sink is called with fDirectLock held.
	void SetDirectSink(PcDirectSink sink)
	{
		std::lock_guard<std::mutex> lock(fDirectLock);
		fDirectSink = sink;
		if (sink != NULL && fDirectReported)
			sink(fId, &fDirect);
	}

	bool DirectConnectedNow()
	{
		std::lock_guard<std::mutex> lock(fDirectLock);
		return fDirect.connected != 0;
	}

	// Called with the window locked: the window fills the monitor it is
	// on, without its border, or goes back to where it was. Its feel stays:
	// a floating full screen window would cover the floating panels' own
	// windows (floating ones too, and the last activated is in front).
	void SetFullscreen(bool fullscreen)
	{
		if (fullscreen == fFullscreen)
			return;
		fFullscreen = fullscreen;
		if (fullscreen) {
			fSavedFrame = Frame();
			fSavedLook = Look();
			BRect monitor = MonitorFrame(this);
			SetLook(B_NO_BORDER_WINDOW_LOOK);
			MoveTo(monitor.LeftTop());
			ResizeTo(monitor.Width(), monitor.Height());
		} else {
			SetLook(fSavedLook);
			MoveTo(fSavedFrame.LeftTop());
			ResizeTo(fSavedFrame.Width(), fSavedFrame.Height());
		}
	}

	bool IsFullscreen() const { return fFullscreen; }

	// The look the window has when it isn't full screen.
	void SetNormalLook(window_look look)
	{
		fSavedLook = look;
		if (!fFullscreen)
			SetLook(look);
	}

	int32 Id() const { return fId; }

private:
	int32 fId;
	PcView* fView;
	BMenuBar* fMenuBar = NULL;
	// Read by the direct daemon thread without the window lock.
	std::atomic<int32_t> fViewTop;

	// The window's last direct connection and where it is reported to (the
	// presenter, src/present.rs).
	std::mutex fDirectLock;
	PcDirectSink fDirectSink;
	PcDirect fDirect;
	bool fDirectReported;
	int32_t fDirectRects[kMaxDirectRects * 4];

	bool fFullscreen;
	BRect fSavedFrame;
	window_look fSavedLook;
};


// The open windows by id. Only the Rust thread adds and removes them. The
// first one is the main window: the menu bar and the file panels go there.
static std::mutex sWindowsLock;
static std::map<int32, PcWindow*> sWindows;
static int32 sNextWindow = 1;
static int32 sMainWindow = 0;


// The window with that id, locked, or NULL. Unlock it when done.
static PcWindow*
LockWindow(int32 id)
{
	PcWindow* window;
	{
		std::lock_guard<std::mutex> lock(sWindowsLock);
		std::map<int32, PcWindow*>::iterator found = sWindows.find(id);
		if (found == sWindows.end())
			return NULL;
		window = found->second;
	}
	return window->Lock() ? window : NULL;
}


static PcWindow*
LockMainWindow()
{
	return LockWindow(sMainWindow);
}


// #pragma mark - File panels


// Shows the files an Open panel offers: folders, and files with one of the
// extensions (all files when there are none).
class ExtensionFilter : public BRefFilter {
public:
	ExtensionFilter(const char* extensions)
	{
		BString list(extensions != NULL ? extensions : "");
		int32 start = 0;
		while (start <= list.Length()) {
			int32 end = list.FindFirst('\n', start);
			if (end < 0)
				end = list.Length();
			BString extension;
			list.CopyInto(extension, start, end - start);
			extension.ToLower();
			if (extension == "*") {
				fExtensions.clear();
				fAll = true;
				return;
			}
			if (extension.Length() > 0)
				fExtensions.push_back(extension);
			start = end + 1;
		}
		fAll = fExtensions.empty();
	}

	virtual bool Filter(const entry_ref* ref, BNode* node,
		struct stat_beos* stat, const char* mimeType)
	{
		if (fAll)
			return true;
		BEntry entry(ref, true);
		if (entry.IsDirectory())
			return true;
		BString name(ref->name);
		name.ToLower();
		for (size_t i = 0; i < fExtensions.size(); i++) {
			BString suffix(".");
			suffix << fExtensions[i];
			if (name.EndsWith(suffix))
				return true;
		}
		return false;
	}

private:
	std::vector<BString> fExtensions;
	bool fAll;
};


static BFilePanel* sOpenPanel = NULL;
static ExtensionFilter* sOpenFilter = NULL;
static BFilePanel* sSavePanel = NULL;
static int32 sDialogId = -1;
static bool sDialogAnswered = true;


static void
AnswerDialog(int32 id, bool picked, const std::string& path)
{
	if (id != sDialogId || sDialogAnswered)
		return;
	sDialogAnswered = true;
	PcEvent event = MakeEvent(PC_DIALOG_ANSWER);
	event.id = id;
	event.x = picked ? 1 : 0;
	event.text_len = path.size();
	Push(event, path);
}


// #pragma mark - Application


class PcApp : public BApplication {
public:
	PcApp(const char* signature, status_t* error)
		:
		BApplication(signature, error),
		fReady(false)
	{
	}

	virtual void ReadyToRun()
	{
		fReady = true;
	}

	virtual void RefsReceived(BMessage* message)
	{
		int32 dialog;
		if (message->FindInt32(kDialogField, &dialog) == B_OK) {
			std::string path = PathsOf(message, true);
			AnswerDialog(dialog, !path.empty(), path);
			return;
		}
		PcEvent event = MakeEvent(PC_OPEN_FILES);
		std::string paths = PathsOf(message, true);
		event.text_len = paths.size();
		if (!paths.empty())
			Push(event, paths);
	}

	virtual void ArgvReceived(int32 argc, char** argv)
	{
		// The Rust side reads its own command line. Another launch's, handed
		// over as this copy is already running (B_SINGLE_LAUNCH), opens its
		// documents here: the arguments that aren't options, relative to the
		// folder it was started in. An automation launch (--control) wants a
		// process of its own and ends with an error instead. BApplication
		// posts this copy's own arguments to itself before B_READY_TO_RUN,
		// without a reply address, so IsSourceRemote() can't tell them from
		// another launch's.
		BMessage* message = CurrentMessage();
		if (!fReady || message == NULL || !message->IsSourceRemote())
			return;
		const char* cwd = NULL;
		message->FindString("cwd", &cwd);
		std::string paths;
		for (int32 i = 1; i < argc; i++) {
			if (strcmp(argv[i], "--control") == 0)
				return;
			if (strcmp(argv[i], "--control-token-file") == 0
				|| strcmp(argv[i], "--automation-read-root") == 0
				|| strcmp(argv[i], "--automation-write-root") == 0) {
				i++;
				continue;
			}
			if (argv[i][0] == '-' && argv[i][1] != '\0')
				continue;
			BPath path;
			if (argv[i][0] != '/' && cwd != NULL)
				path.SetTo(cwd, argv[i], true);
			else
				path.SetTo(argv[i], NULL, true);
			if (path.InitCheck() != B_OK)
				continue;
			if (!paths.empty())
				paths += '\n';
			paths += path.Path();
		}
		// Even without documents: the running copy's window comes to the
		// front, as BApplication does for a launch without arguments
		// (B_SILENT_RELAUNCH).
		PcEvent event = MakeEvent(PC_OPEN_FILES);
		event.text_len = paths.size();
		Push(event, paths);
	}

	virtual bool QuitRequested()
	{
		if (sAllowQuit)
			return BApplication::QuitRequested();
		Push(MakeEvent(PC_QUIT_REQUESTED));
		return false;
	}

	virtual void MessageReceived(BMessage* message)
	{
		switch (message->what) {
			case B_SAVE_REQUESTED:
			{
				int32 dialog;
				entry_ref directory;
				const char* name;
				if (message->FindInt32(kDialogField, &dialog) != B_OK)
					break;
				if (message->FindRef("directory", &directory) != B_OK
					|| message->FindString("name", &name) != B_OK) {
					AnswerDialog(dialog, false, std::string());
					break;
				}
				BPath path(&directory);
				path.Append(name);
				AnswerDialog(dialog, path.InitCheck() == B_OK,
					path.InitCheck() == B_OK ? path.Path() : "");
				break;
			}
			case B_CANCEL:
				// Sent whenever a panel closes, after its answer if it had
				// one.
				AnswerDialog(sDialogId, false, std::string());
				break;
			case B_SILENT_RELAUNCH:
				// A launch without arguments while this copy runs (the
				// Deskbar, Tracker's icon): BApplication only activates the
				// team, which leaves a minimized window down. The window
				// comes forward as for opened documents.
				Push(MakeEvent(PC_OPEN_FILES));
				BApplication::MessageReceived(message);
				break;
			default:
				BApplication::MessageReceived(message);
		}
	}

private:
	// B_READY_TO_RUN has come: B_ARGV_RECEIVED is another launch's.
	bool fReady;
};


// #pragma mark - Clipboard


static void*
CopyOut(const void* data, size_t length, size_t* _length)
{
	void* copy = malloc(length > 0 ? length : 1);
	if (copy == NULL)
		return NULL;
	memcpy(copy, data, length);
	*_length = length;
	return copy;
}


// #pragma mark - Native menu bar


// A menu item whose shortcut BMenuItem can't draw (it draws one character:
// not F5, Page Up or Home): the shortcut is drawn as text at its right end.
class PcMenuItem : public BMenuItem {
public:
	PcMenuItem(const char* label, BMessage* message, char shortcut,
		uint32 modifiers, const char* keyText)
		:
		BMenuItem(label, message, shortcut, modifiers),
		fKeyText(keyText)
	{
	}

protected:
	virtual void GetContentSize(float* width, float* height)
	{
		BMenuItem::GetContentSize(width, height);
		if (fKeyText.Length() > 0 && Menu() != NULL) {
			*width += Menu()->StringWidth(fKeyText.String())
				+ be_plain_font->Size() * 2;
		}
	}

	virtual void DrawContent()
	{
		BMenuItem::DrawContent();
		BMenu* menu = Menu();
		if (fKeyText.Length() == 0 || menu == NULL)
			return;
		font_height height;
		menu->GetFontHeight(&height);
		BRect frame = Frame();
		float width = menu->StringWidth(fKeyText.String());
		menu->DrawString(fKeyText.String(),
			BPoint(frame.right - width - be_plain_font->Size() * 0.75f,
				ContentLocation().y + ceilf(height.ascent)));
	}

private:
	BString fKeyText;
};


// The items of the menu bar pc_menu_build made last, by index (NULL for the
// window's own items).
static std::vector<BMenuItem*> sMenuItems;


// The fields of one line of a menu spec (tab-separated).
static std::vector<BString>
SplitTabs(const BString& line)
{
	std::vector<BString> fields;
	int32 start = 0;
	while (true) {
		int32 end = line.FindFirst('\t', start);
		BString field;
		line.CopyInto(field, start, (end < 0 ? line.Length() : end) - start);
		fields.push_back(field);
		if (end < 0)
			break;
		start = end + 1;
	}
	return fields;
}


// Builds a menu bar from a spec, one line per entry (src/native_menu.rs):
//   M <title>                     a menu, until its E
//   S <label>                     a submenu, until its E
//   E                             the end of a menu or submenu
//   -                             a separator
//   I <index> <label> <char> <modifiers> <key text> <enabled> <checked>
//                                 an item (char: its shortcut's character
//                                 code, 0 for none; checked: -1 for an item
//                                 that is never checked)
//   W <min|zoom|close> <label>    one of the window's own items
static BMenuBar*
BuildMenuBar(const char* spec)
{
	BMenuBar* bar = new BMenuBar(BRect(0, 0, 100, 10), "menu bar",
		B_FOLLOW_LEFT_RIGHT | B_FOLLOW_TOP);
	std::vector<BMenu*> stack;
	sMenuItems.clear();
	BString text(spec);
	int32 start = 0;
	while (start < text.Length()) {
		int32 end = text.FindFirst('\n', start);
		if (end < 0)
			end = text.Length();
		BString line;
		text.CopyInto(line, start, end - start);
		start = end + 1;
		std::vector<BString> f = SplitTabs(line);
		if (f.empty() || f[0].Length() != 1)
			continue;
		BMenu* menu = stack.empty() ? NULL : stack.back();
		switch (f[0][0]) {
			case 'M':
			case 'S':
			{
				BMenu* sub = new BMenu(f.size() > 1 ? f[1].String() : "");
				if (menu != NULL)
					menu->AddItem(sub);
				else
					bar->AddItem(sub);
				stack.push_back(sub);
				break;
			}
			case 'E':
				if (!stack.empty())
					stack.pop_back();
				break;
			case '-':
				if (menu != NULL)
					menu->AddSeparatorItem();
				break;
			case 'I':
			{
				if (menu == NULL || f.size() < 8)
					break;
				int32 index = atoi(f[1].String());
				BMessage* message = new BMessage(kMenuItem);
				message->AddInt32("index", index);
				PcMenuItem* item = new PcMenuItem(f[2].String(), message,
					(char)atoi(f[3].String()), (uint32)atoi(f[4].String()),
					f[5].String());
				item->SetEnabled(atoi(f[6].String()) != 0);
				item->SetMarked(atoi(f[7].String()) > 0);
				menu->AddItem(item);
				if (index >= 0 && index < 100000) {
					if ((size_t)index >= sMenuItems.size())
						sMenuItems.resize(index + 1, NULL);
					sMenuItems[index] = item;
				}
				break;
			}
			case 'W':
			{
				if (menu == NULL || f.size() < 3)
					break;
				uint32 what = f[1] == "min" ? kMenuMinimize
					: f[1] == "zoom" ? kMenuZoom : kMenuClose;
				menu->AddItem(new BMenuItem(f[2].String(), new BMessage(what),
					what == kMenuMinimize ? 'M' : what == kMenuClose ? 'W' : 0));
				break;
			}
		}
	}
	return bar;
}


// #pragma mark - Screen colour picker


static const uint32 kPickerZoomMin = 4;
static const uint32 kPickerZoomMax = 24;

// The pick in progress (pc_host_screen_pick); the picker's window answers it
// from its own thread.
static std::mutex sPickLock;
static int32 sPickId = -1;
static bool sPickAnswered = true;
static BMessenger sPicker;


// Hands the picker's answer to the interface (once per pick): the colour as
// 0xRRGGBB in buttons and the screen point it was at, or picked 0.
static void
AnswerPick(int32 id, bool picked, uint32 rgb, BPoint where)
{
	{
		std::lock_guard<std::mutex> lock(sPickLock);
		if (id != sPickId || sPickAnswered)
			return;
		sPickAnswered = true;
	}
	PcEvent event = MakeEvent(PC_SCREEN_PICK);
	event.id = id;
	event.x = picked ? 1 : 0;
	event.buttons = (int32)rgb;
	event.dx = where.x;
	event.dy = where.y;
	Push(event);
}


// Shows a snapshot of the screen (its view bitmap) under a magnifying loupe.
// A click picks the pixel under the pointer; Esc or a right click cancels, as
// in the interface's own picker (crates/ui-egui/src/dialogs/screen_picker.rs).
class PickerView : public BView {
public:
	PickerView(BRect frame, BBitmap* snapshot, int32 id, const char* hint,
		const char* wheelHint)
		:
		BView(frame, "picker", B_FOLLOW_ALL_SIDES, B_WILL_DRAW),
		fSnapshot(snapshot),
		fId(id),
		fHint(hint),
		fWheelHint(wheelHint),
		fZoom(12),
		fWhere(-1, -1),
		fPressed(0),
		fCursor(B_CURSOR_ID_CROSS_HAIR)
	{
	}

	~PickerView()
	{
		delete fSnapshot;
	}

	virtual void AttachedToWindow()
	{
		// The snapshot is drawn by app_server, under the loupe.
		SetViewBitmap(fSnapshot, fSnapshot->Bounds(), Bounds(),
			B_FOLLOW_TOP | B_FOLLOW_LEFT, 0);
		SetViewCursor(&fCursor);
		MakeFocus(true);
		BPoint where;
		uint32 buttons;
		GetMouse(&where, &buttons, false);
		fWhere = where;
	}

	virtual void Draw(BRect updateRect)
	{
		if (!Bounds().Contains(fWhere))
			return;
		BRect box = Loupe();
		rgb_color panel = ui_color(B_MENU_BACKGROUND_COLOR);
		rgb_color text = ui_color(B_MENU_ITEM_TEXT_COLOR);
		SetHighColor(panel);
		FillRect(box);
		SetHighColor(tint_color(panel, B_DARKEN_2_TINT));
		StrokeRect(box);
		int32 px, py;
		Pixel(fWhere, px, py);
		BPoint origin = box.LeftTop() + BPoint(
			floorf((box.Width() - 11 * fZoom) / 2), 6);
		for (int32 row = 0; row < 11; row++) {
			for (int32 col = 0; col < 11; col++) {
				rgb_color color;
				if (!Color(px + col - 5, py + row - 5, color))
					color = panel;
				SetHighColor(color);
				FillRect(BRect(origin.x + col * fZoom, origin.y + row * fZoom,
					origin.x + (col + 1) * fZoom - 1,
					origin.y + (row + 1) * fZoom - 1));
			}
		}
		BRect center(origin.x + 5 * fZoom, origin.y + 5 * fZoom,
			origin.x + 6 * fZoom - 1, origin.y + 6 * fZoom - 1);
		SetHighColor(0, 0, 0);
		StrokeRect(center.InsetByCopy(-1, -1));
		SetHighColor(255, 255, 255);
		StrokeRect(center);
		rgb_color picked;
		BString label;
		if (Color(px, py, picked))
			label.SetToFormat("#%02X%02X%02X   %" B_PRIu32 "\xC3\x97", picked.red,
				picked.green, picked.blue, fZoom);
		font_height height;
		GetFontHeight(&height);
		float line = ceilf(height.ascent + height.descent + height.leading);
		SetHighColor(text);
		SetLowColor(panel);
		BPoint at(box.left + 8, origin.y + 11 * fZoom + 8 + height.ascent);
		DrawString(label.String(), at);
		DrawString(fHint.String(), at + BPoint(0, line));
		DrawString(fWheelHint.String(), at + BPoint(0, 2 * line));
	}

	virtual void MouseMoved(BPoint where, uint32 transit,
		const BMessage* dragMessage)
	{
		Invalidate(Loupe());
		fWhere = where;
		Invalidate(Loupe());
	}

	virtual void MouseDown(BPoint where)
	{
		SetMouseEventMask(B_POINTER_EVENTS, B_NO_POINTER_HISTORY);
		int32 buttons = 0;
		Window()->CurrentMessage()->FindInt32("buttons", &buttons);
		fPressed |= buttons;
		if ((buttons & B_SECONDARY_MOUSE_BUTTON) != 0)
			Finish(false, where);
	}

	virtual void MouseUp(BPoint where)
	{
		// A press that moved picks where it is released.
		if ((fPressed & B_PRIMARY_MOUSE_BUTTON) != 0)
			Finish(true, where);
	}

	virtual void KeyDown(const char* bytes, int32 numBytes)
	{
		if (numBytes == 1 && bytes[0] == B_ESCAPE)
			Finish(false, fWhere);
		else
			BView::KeyDown(bytes, numBytes);
	}

	virtual void MessageReceived(BMessage* message)
	{
		if (message->what == B_MOUSE_WHEEL_CHANGED) {
			float delta = 0;
			message->FindFloat("be:wheel_delta_y", &delta);
			Invalidate(Loupe());
			if (delta < 0 && fZoom + 2 <= kPickerZoomMax)
				fZoom += 2;
			else if (delta > 0 && fZoom - 2 >= kPickerZoomMin)
				fZoom -= 2;
			Invalidate(Loupe());
			return;
		}
		BView::MessageReceived(message);
	}

	void Finish(bool pick, BPoint where)
	{
		int32 px, py;
		rgb_color color;
		Pixel(where, px, py);
		bool picked = pick && Bounds().Contains(where) && Color(px, py, color);
		uint32 rgb = picked ? ((uint32)color.red << 16)
			| ((uint32)color.green << 8) | color.blue : 0;
		AnswerPick(fId, picked, rgb, ConvertToScreen(where));
		Window()->PostMessage(B_QUIT_REQUESTED);
	}

private:
	// The loupe's box: below and right of the pointer, kept on the screen.
	BRect Loupe() const
	{
		float width = std::max(11.0f * fZoom + 12,
			std::max(StringWidth(fHint.String()), StringWidth(fWheelHint.String()))
				+ 16);
		font_height height;
		GetFontHeight(&height);
		float line = ceilf(height.ascent + height.descent + height.leading);
		BRect box(0, 0, width, 11 * fZoom + 14 + 3 * line);
		BRect bounds = Bounds();
		box.OffsetTo(std::max(bounds.left,
				std::min(fWhere.x + 24, bounds.right - box.Width())),
			std::max(bounds.top,
				std::min(fWhere.y + 24, bounds.bottom - box.Height())));
		return box;
	}

	// The snapshot's pixel under a point of the view (the view may be
	// logical pixels and the snapshot device pixels).
	void Pixel(BPoint where, int32& x, int32& y) const
	{
		BRect bounds = Bounds();
		BRect source = fSnapshot->Bounds();
		x = (int32)floorf((where.x - bounds.left) * (source.Width() + 1)
			/ (bounds.Width() + 1));
		y = (int32)floorf((where.y - bounds.top) * (source.Height() + 1)
			/ (bounds.Height() + 1));
	}

	bool Color(int32 x, int32 y, rgb_color& color) const
	{
		BRect source = fSnapshot->Bounds();
		if (x < 0 || y < 0 || x > source.IntegerWidth()
			|| y > source.IntegerHeight())
			return false;
		const uint8* bits = (const uint8*)fSnapshot->Bits()
			+ (size_t)y * fSnapshot->BytesPerRow() + (size_t)x * 4;
		color.blue = bits[0];
		color.green = bits[1];
		color.red = bits[2];
		color.alpha = 255;
		return true;
	}

	BBitmap* fSnapshot;
	int32 fId;
	BString fHint;
	BString fWheelHint;
	uint32 fZoom;
	BPoint fWhere;
	int32 fPressed;
	BCursor fCursor;
};


class PickerWindow : public BWindow {
public:
	PickerWindow(BRect frame, BBitmap* snapshot, int32 id, const char* hint,
		const char* wheelHint)
		:
		BWindow(frame, "Screen Color Picker", B_NO_BORDER_WINDOW_LOOK,
			B_FLOATING_ALL_WINDOW_FEEL, B_NOT_MOVABLE | B_NOT_RESIZABLE
				| B_NOT_ZOOMABLE | B_NOT_MINIMIZABLE | B_ASYNCHRONOUS_CONTROLS),
		fId(id)
	{
		AddChild(new PickerView(Bounds(), snapshot, id, hint, wheelHint));
	}

	virtual void WindowActivated(bool active)
	{
		if (active && ChildAt(0) != NULL)
			ChildAt(0)->MakeFocus(true);
	}

	virtual bool QuitRequested()
	{
		// Closed any other way (pc_host_screen_pick_cancel): no colour.
		AnswerPick(fId, false, 0, BPoint(0, 0));
		return true;
	}

private:
	int32 fId;
};


// #pragma mark - C interface


extern "C" {


// Creates the application: 0, 1 when a copy started from the same file is
// already running (B_SINGLE_LAUNCH) and has been handed this launch's
// arguments, or -1.
int32_t
pc_host_init(const char* signature)
{
	if (be_app != NULL)
		return 0;
	// Not deleted when it failed: the process ends.
	status_t error = B_OK;
	new PcApp(signature, &error);
	if (error == B_ALREADY_RUNNING)
		return 1;
	return error == B_OK && be_app != NULL ? 0 : -1;
}


// How pc_host_open_window opens a window (its flags).
enum {
	PC_WINDOW_UNDECORATED = 1,
	PC_WINDOW_FIXED_SIZE = 2,
	PC_WINDOW_HIDDEN = 4,
	PC_WINDOW_ZOOMED = 8,
	PC_WINDOW_NO_CLOSE = 16,
	PC_WINDOW_NO_MINIMIZE = 32,
	PC_WINDOW_NO_ZOOM = 64,
	PC_WINDOW_FLOATING = 128,
	PC_WINDOW_FULLSCREEN = 256,
};


static uint32
WindowFlags(int32_t flags)
{
	uint32 windowFlags = 0;
	if ((flags & PC_WINDOW_FIXED_SIZE) != 0)
		windowFlags |= B_NOT_RESIZABLE | B_NOT_ZOOMABLE;
	if ((flags & PC_WINDOW_NO_CLOSE) != 0)
		windowFlags |= B_NOT_CLOSABLE;
	if ((flags & PC_WINDOW_NO_MINIMIZE) != 0)
		windowFlags |= B_NOT_MINIMIZABLE;
	if ((flags & PC_WINDOW_NO_ZOOM) != 0)
		windowFlags |= B_NOT_ZOOMABLE;
	return windowFlags;
}


// Opens a window and returns its id (0 when it couldn't): its frame's
// top-left corner (border and tab included) at (x, y), centred on the monitor
// of the active window (or the screen) when x is negative, content size
// width × height, its smallest and largest sizes (0: none) and the
// PC_WINDOW_* flags. The first window is the main window.
int32_t
pc_host_open_window(const char* title, float x, float y, float width,
	float height, float minWidth, float minHeight, float maxWidth,
	float maxHeight, int32_t flags)
{
	BRect frame(0, 0, width - 1, height - 1);
	BScreen screen;
	BRect screenFrame = screen.Frame();
	if (x >= 0 && y >= 0)
		frame.OffsetTo(x, y);
	int32 id;
	{
		std::lock_guard<std::mutex> lock(sWindowsLock);
		id = sNextWindow++;
	}
	PcWindow* window = new PcWindow(id, frame, title,
		(flags & PC_WINDOW_UNDECORATED) != 0
			? B_NO_BORDER_WINDOW_LOOK : B_TITLED_WINDOW_LOOK,
		(flags & PC_WINDOW_FLOATING) != 0
			? B_FLOATING_ALL_WINDOW_FEEL : B_NORMAL_WINDOW_FEEL,
		WindowFlags(flags));
	minWidth = std::max(minWidth, 1.0f);
	minHeight = std::max(minHeight, 1.0f);
	window->SetSizeLimits(minWidth - 1, maxWidth > 0 ? maxWidth - 1 : 32768,
		minHeight - 1, maxHeight > 0 ? maxHeight - 1 : 32768);
	if (x < 0 || y < 0 || !screenFrame.Intersects(frame)) {
		// The monitor of the active window (on air/OS the screen spans every
		// monitor), or the one the window is on.
		BRect monitor = MonitorFrame(window);
		{
			std::lock_guard<std::mutex> lock(sWindowsLock);
			for (std::map<int32, PcWindow*>::iterator it = sWindows.begin();
					it != sWindows.end(); it++) {
				if (it->second->Lock()) {
					if (it->second->IsActive())
						monitor = MonitorFrame(it->second);
					it->second->Unlock();
				}
			}
		}
		// A default size larger than the monitor (a small or dense display)
		// shrinks to fit it, with room for the tab, borders and Deskbar.
		float fitWidth = monitor.Width() * 0.92f;
		float fitHeight = monitor.Height() * 0.88f;
		if (frame.Width() > fitWidth || frame.Height() > fitHeight) {
			window->ResizeTo(std::max(std::min(frame.Width(), fitWidth),
					minWidth - 1),
				std::max(std::min(frame.Height(), fitHeight), minHeight - 1));
		}
		BRect outer = window->DecoratorFrame();
		window->MoveBy(roundf(monitor.left + (monitor.Width() - outer.Width()) / 2) - outer.left,
			roundf(monitor.top + (monitor.Height() - outer.Height()) / 2) - outer.top);
	} else {
		// (x, y) is where the frame's corner goes, tab and border included.
		BRect outer = window->DecoratorFrame();
		window->MoveBy(frame.left - outer.left, frame.top - outer.top);
	}
	{
		std::lock_guard<std::mutex> lock(sWindowsLock);
		sWindows[id] = window;
		if (sMainWindow == 0)
			sMainWindow = id;
	}
	if ((flags & PC_WINDOW_ZOOMED) != 0)
		window->Zoom();
	if ((flags & PC_WINDOW_FULLSCREEN) != 0)
		window->SetFullscreen(true);
	// A window made hidden stays locked by its maker (as BWindow's
	// constructor leaves it) until it is first shown.
	if ((flags & PC_WINDOW_HIDDEN) == 0)
		window->Show();
	return id;
}


// Closes a window pc_host_open_window opened. Its direct connection's sink
// should be unset first.
void
pc_host_close_window(int32_t id)
{
	PcWindow* window;
	{
		std::lock_guard<std::mutex> lock(sWindowsLock);
		std::map<int32, PcWindow*>::iterator found = sWindows.find(id);
		if (found == sWindows.end())
			return;
		window = found->second;
		sWindows.erase(found);
	}
	if (window->Lock())
		window->Quit();
}


void
pc_host_run()
{
	be_app->Run();
	delete be_app;
}


// Called by the Rust thread once it is done: its windows close (a window it
// made hidden is still locked by it), and the application quits.
void
pc_host_quit()
{
	std::vector<int32> ids;
	{
		std::lock_guard<std::mutex> lock(sWindowsLock);
		for (std::map<int32, PcWindow*>::iterator it = sWindows.begin();
				it != sWindows.end(); it++)
			ids.push_back(it->first);
	}
	sAllowQuit = true;
	for (size_t i = 0; i < ids.size(); i++)
		pc_host_close_window(ids[i]);
	be_app->PostMessage(B_QUIT_REQUESTED);
}


// Waits until an event is queued, pc_host_wake is called or the timeout
// (microseconds; negative: none) passes. Returns how many events are queued.
int32_t
pc_host_wait(int64_t timeout)
{
	std::unique_lock<std::mutex> lock(sQueueLock);
	auto ready = [] { return !sQueue.empty() || sWoken; };
	if (timeout < 0)
		sQueueCondition.wait(lock, ready);
	else if (timeout > 0)
		sQueueCondition.wait_for(lock, std::chrono::microseconds(timeout), ready);
	sWoken = false;
	return (int32_t)sQueue.size();
}


void
pc_host_wake()
{
	{
		std::lock_guard<std::mutex> lock(sQueueLock);
		sWoken = true;
	}
	sQueueCondition.notify_all();
}


// Takes the next event, if any (1) or returns 0. Its text stays readable
// through pc_host_event_text until the next call.
int32_t
pc_host_next_event(PcEvent* event)
{
	std::lock_guard<std::mutex> lock(sQueueLock);
	if (sQueue.empty())
		return 0;
	*event = sQueue.front().event;
	sCurrentText = sQueue.front().text;
	event->text_len = sCurrentText.size();
	sQueue.pop_front();
	return 1;
}


const char*
pc_host_event_text()
{
	return sCurrentText.c_str();
}


void
pc_host_present(int32_t id, const uint32_t* pixels, int32_t width,
	int32_t height, int32_t x0, int32_t y0, int32_t x1, int32_t y1,
	float scale)
{
	if (width <= 0 || height <= 0 || scale <= 0)
		return;
	PcWindow* window = LockWindow(id);
	if (window == NULL)
		return;
	window->View()->Present(pixels, width, height, x0, y0, x1, y1, scale);
	window->Unlock();
}


// The rectangles ([x0, y0, x1, y1) device pixels, four values each) of a
// frame read back from the GPU.
void
pc_host_present_rects(int32_t id, const uint32_t* pixels, int32_t width,
	int32_t height, const int32_t* rects, int32_t count, float scale)
{
	if (width <= 0 || height <= 0 || scale <= 0 || count <= 0)
		return;
	PcWindow* window = LockWindow(id);
	if (window == NULL)
		return;
	window->View()->PresentRects(pixels, width, height, rects, count, scale,
		window->DirectConnectedNow());
	window->Unlock();
}


// Where a window's direct connection is reported (NULL: nowhere). The last
// one is reported at once; unsetting waits for a report in progress.
void
pc_host_direct_sink(int32_t id, PcDirectSink sink)
{
	PcWindow* window;
	{
		std::lock_guard<std::mutex> lock(sWindowsLock);
		std::map<int32, PcWindow*>::iterator found = sWindows.find(id);
		if (found == sWindows.end())
			return;
		window = found->second;
	}
	// Not with the window locked: the direct daemon thread reports without
	// it, and app_server waits for that while the window may wait for us.
	window->SetDirectSink(sink);
}


void
pc_host_window_info(int32_t id, PcWindowInfo* info)
{
	memset(info, 0, sizeof(*info));
	PcWindow* window = LockWindow(id);
	if (window == NULL)
		return;
	BRect inner = window->Frame();
	BRect outer = window->DecoratorFrame();
	BRect monitor = MonitorFrame(window);
	// The interface's viewport is the canvas view, below the menu bar.
	info->inner_left = inner.left;
	info->inner_top = inner.top + window->ViewTop();
	info->inner_right = inner.right + 1;
	info->inner_bottom = inner.bottom + 1;
	info->outer_left = outer.left;
	info->outer_top = outer.top;
	info->outer_right = outer.right + 1;
	info->outer_bottom = outer.bottom + 1;
	info->monitor_left = monitor.left;
	info->monitor_top = monitor.top;
	info->monitor_right = monitor.right + 1;
	info->monitor_bottom = monitor.bottom + 1;
	info->minimized = window->IsMinimized();
	info->active = window->IsActive();
	// Haiku doesn't say whether a window is zoomed.
	info->zoomed = 0;
	info->fullscreen = window->IsFullscreen();
	info->hidden = window->IsHidden();
	window->Unlock();
}


void
pc_host_set_title(int32_t id, const char* title)
{
	PcWindow* window = LockWindow(id);
	if (window == NULL)
		return;
	if (strcmp(window->Title(), title) != 0)
		window->SetTitle(title);
	window->Unlock();
}


// The window commands of src/host.rs's `window_command`. Sizes are the
// canvas's: the main window's menu bar comes on top.
void
pc_host_window_command(int32_t id, int32_t command, float a, float b)
{
	PcWindow* window = LockWindow(id);
	if (window == NULL)
		return;
	int32 top = window->ViewTop();
	switch (command) {
		case 1:
		{
			// Move the frame's top-left corner (decorator included) to (a, b).
			BRect inner = window->Frame();
			BRect outer = window->DecoratorFrame();
			window->MoveTo(a + inner.left - outer.left, b + inner.top - outer.top);
			break;
		}
		case 2:
			window->ResizeTo(a - 1, b - 1 + top);
			break;
		case 3:
			window->Minimize(a != 0);
			break;
		case 4:
			// Zoom toggles: only zoom, never un-zoom on request.
			if (a != 0)
				window->Zoom();
			break;
		case 5:
			window->Activate(true);
			break;
		case 6:
		case 14:
		{
			float minWidth, maxWidth, minHeight, maxHeight;
			window->GetSizeLimits(&minWidth, &maxWidth, &minHeight, &maxHeight);
			if (command == 6) {
				window->SetSizeLimits(std::max(a, 1.0f) - 1, maxWidth,
					std::max(b, 1.0f) - 1 + top, maxHeight);
			} else {
				window->SetSizeLimits(minWidth, a > 0 ? a - 1 : 32768,
					minHeight, b > 0 ? b - 1 + top : 32768);
			}
			break;
		}
		case 7:
			if (a != 0 && window->IsHidden())
				window->Show();
			else if (a == 0 && !window->IsHidden())
				window->Hide();
			break;
		case 8:
			window->SetNormalLook(a != 0
				? B_TITLED_WINDOW_LOOK : B_NO_BORDER_WINDOW_LOOK);
			break;
		case 9:
		{
			uint32 flags = window->Flags();
			if (a != 0)
				flags &= ~(B_NOT_RESIZABLE | B_NOT_ZOOMABLE);
			else
				flags |= B_NOT_RESIZABLE | B_NOT_ZOOMABLE;
			window->SetFlags(flags);
			break;
		}
		case 10:
			window->SetFeel(a == 1
				? B_FLOATING_ALL_WINDOW_FEEL : B_NORMAL_WINDOW_FEEL);
			break;
		case 11:
			window->SetFullscreen(a != 0);
			break;
		case 12:
			window->View()->StartDrag(0);
			break;
		case 13:
			window->View()->StartDrag((int32)a);
			break;
		case 15:
		{
			// The title bar's buttons: 1 close, 2 minimize, 4 zoom.
			int32 buttons = (int32)a;
			uint32 flags = window->Flags()
				& ~(B_NOT_CLOSABLE | B_NOT_MINIMIZABLE | B_NOT_ZOOMABLE);
			if ((buttons & 1) == 0)
				flags |= B_NOT_CLOSABLE;
			if ((buttons & 2) == 0)
				flags |= B_NOT_MINIMIZABLE;
			if ((buttons & 4) == 0)
				flags |= B_NOT_ZOOMABLE;
			window->SetFlags(flags);
			break;
		}
	}
	window->Unlock();
}


void
pc_host_set_cursor(int32_t id, int32_t cursor)
{
	PcWindow* window = LockWindow(id);
	if (window == NULL)
		return;
	window->View()->SetCursorId(cursor);
	window->Unlock();
}


uint32_t
pc_host_modifiers()
{
	return modifiers();
}


// The interface scale: the system font size against Haiku's default 12 pt.
float
pc_host_ui_scale()
{
	float scale = be_plain_font->Size() / 12.0f;
	if (scale < 1.0f)
		scale = 1.0f;
	if (scale > 4.0f)
		scale = 4.0f;
	return scale;
}


// How many frame buffer pixels a logical pixel is: air/OS draws the desktop
// at its monitors' density (the "render scale" of its display layout, in
// percent). 1 on a Haiku without the call.
float
pc_host_device_scale()
{
	typedef status_t (*GetLayout)(BMessage&);
	static GetLayout getLayout = (GetLayout)dlsym(RTLD_DEFAULT,
		"_ZN8BPrivate18get_display_layoutER8BMessage");
	if (getLayout == NULL)
		return 1;
	BMessage layout;
	if (getLayout(layout) != B_OK)
		return 1;
	int32 best = 0;
	BMessage display;
	for (int32 i = 0; layout.FindMessage("display", i, &display) == B_OK; i++) {
		int32 scale;
		if (display.GetBool("enabled", true)
			&& display.FindInt32("render scale", &scale) == B_OK && scale > best)
			best = scale;
	}
	if (best < 100 || best > 400)
		return 1;
	return best / 100.0f;
}


// Whether the panel colour (Appearance preferences) is dark.
int32_t
pc_host_dark()
{
	rgb_color color = ui_color(B_PANEL_BACKGROUND_COLOR);
	return color.Brightness() < 128 ? 1 : 0;
}


// The system's formatting conventions (en_US) and the preferred languages
// (en, de…, one per line), for the locale variables the interface reads.
char*
pc_host_locale(char** languages)
{
	BFormattingConventions conventions;
	BString id;
	BLocaleRoster* roster = BLocaleRoster::Default();
	if (roster != NULL) {
		const BLocale* locale = roster->GetDefaultLocale();
		if (locale != NULL && locale->GetFormattingConventions(&conventions) == B_OK)
			id = conventions.ID();
	}
	BString list;
	BMessage preferred;
	if (roster != NULL && roster->GetPreferredLanguages(&preferred) == B_OK) {
		const char* language;
		for (int32 i = 0; preferred.FindString("language", i, &language) == B_OK; i++) {
			if (list.Length() > 0)
				list << "\n";
			list << language;
		}
	}
	*languages = strdup(list.String());
	return strdup(id.String());
}


void
pc_free(void* data)
{
	free(data);
}


// The clipboard's data of one MIME type (malloc'd; NULL when absent).
void*
pc_clipboard_get(const char* type, size_t* length)
{
	*length = 0;
	void* copy = NULL;
	BMessage bitmapArchive;
	bool nativeBitmap = false;
	if (!be_clipboard->Lock())
		return NULL;
	BMessage* clip = be_clipboard->Data();
	const void* data;
	ssize_t size;
	if (clip != NULL && clip->FindData(type, B_MIME_TYPE, &data, &size) == B_OK
		&& size >= 0 && size <= (512LL << 20))
		copy = CopyOut(data, size, length);
	else if (clip != NULL && strcmp(type, "image/png") == 0)
		nativeBitmap = clip->FindMessage("image/bitmap", &bitmapArchive) == B_OK;
	be_clipboard->Unlock();
	// ShowImage and WonderBrush exchange archived BBitmaps. Translate outside
	// the clipboard lock so another application's clipboard stays responsive.
	BRect bounds;
	int32 rowBytes = 0;
	bitmapArchive.FindInt32("_rowbytes", &rowBytes);
	if (copy == NULL && nativeBitmap
		&& bitmapArchive.FindRect("_frame", &bounds) == B_OK
		&& bounds.IsValid() && isfinite(bounds.Width()) && isfinite(bounds.Height())
		&& bounds.Width() < 32768 && bounds.Height() < 32768
		&& (bounds.Width() + 1) * (bounds.Height() + 1) <= (128LL << 20)
		&& rowBytes >= 0 && double(rowBytes) * (bounds.Height() + 1) <= (512LL << 20)) {
		BBitmap* bitmap = new BBitmap(&bitmapArchive);
		if (bitmap->InitCheck() == B_OK) {
			BBitmapStream source(bitmap); // Owns bitmap.
			BMallocIO png;
			if (BTranslatorRoster::Default()->Translate(&source, NULL, NULL, &png,
				B_PNG_FORMAT) == B_OK)
				copy = CopyOut(png.Buffer(), png.BufferLength(), length);
		} else {
			delete bitmap;
		}
	}
	return copy;
}




// Replaces the clipboard with text and, when html isn't NULL, its HTML.
int32_t
pc_clipboard_set(const char* text, size_t textLength, const char* html,
	size_t htmlLength)
{
	if (!be_clipboard->Lock())
		return -1;
	be_clipboard->Clear();
	BMessage* clip = be_clipboard->Data();
	status_t status = B_ERROR;
	if (clip != NULL) {
		status = clip->AddData("text/plain", B_MIME_TYPE, text, textLength);
		if (status == B_OK && html != NULL)
			status = clip->AddData("text/html", B_MIME_TYPE, html, htmlLength);
		if (status == B_OK)
			status = be_clipboard->Commit();
	}
	be_clipboard->Unlock();
	return status == B_OK ? 0 : -1;
}


// Puts an encoded picture (PNG) on the clipboard.
int32_t
pc_clipboard_set_picture(const char* type, const void* data, size_t length)
{
	BMessage bitmapArchive;
	bool nativeBitmap = false;
	if (strcmp(type, "image/png") == 0) {
		BMemoryIO input(data, length);
		BBitmap* bitmap = BTranslationUtils::GetBitmap(&input);
		if (bitmap != NULL) {
			nativeBitmap = bitmap->Archive(&bitmapArchive) == B_OK;
			delete bitmap;
		}
	}
	if (!be_clipboard->Lock())
		return -1;
	be_clipboard->Clear();
	BMessage* clip = be_clipboard->Data();
	status_t status = B_ERROR;
	if (clip != NULL) {
		status = clip->AddData(type, B_MIME_TYPE, data, length);
		if (status == B_OK && nativeBitmap)
			status = clip->AddMessage("image/bitmap", &bitmapArchive);
		if (status == B_OK)
			status = be_clipboard->Commit();
	}
	be_clipboard->Unlock();
	return status == B_OK ? 0 : -1;
}




// Keep file panels on the same monitor as the document window.
static void
PositionPanel(BFilePanel* panel)
{
    PcWindow* main = LockMainWindow();
    if (main == NULL) return;
    BRect parent = main->Frame();
    BRect screen = MonitorFrame(main);
    main->Unlock();
    BWindow* window = panel->Window();
    if (!window->Lock()) return;
    BRect frame = window->Frame();
    float x = parent.left + (parent.Width() - frame.Width()) / 2;
    float y = parent.top + (parent.Height() - frame.Height()) / 2;
    window->MoveTo(std::max(screen.left, std::min(x, screen.right - frame.Width())),
        std::max(screen.top + 25, std::min(y, screen.bottom - frame.Height())));
    window->Unlock();
}

// Shows a file panel: Open (save == 0) offering the extensions (one per line;
// "*": any file), or Save suggesting the name. Its answer arrives as a
// PC_DIALOG_ANSWER event with the id.
int32_t
pc_host_file_panel(int32_t id, int32_t save, const char* name,
	const char* extensions, const char* title)
{
	BMessenger target(be_app);
	if (save == 1) {
		if (sSavePanel == NULL) {
			sSavePanel = new BFilePanel(B_SAVE_PANEL, &target, NULL, 0, false,
				NULL, NULL, false, true);
		}
		BMessage message(B_SAVE_REQUESTED);
		message.AddInt32(kDialogField, id);
		sSavePanel->SetMessage(&message);
		BPath suggested(name != NULL ? name : "");
		BPath parent;
		if (name != NULL && strchr(name, '/') != NULL
			&& suggested.GetParent(&parent) == B_OK) {
			sSavePanel->SetPanelDirectory(parent.Path());
			sSavePanel->SetSaveText(suggested.Leaf());
		} else {
			sSavePanel->SetSaveText(name != NULL ? name : "");
		}
		if (sSavePanel->Window()->Lock()) {
			sSavePanel->Window()->SetTitle(title);
			sSavePanel->Window()->Unlock();
		}
		sDialogId = id;
		sDialogAnswered = false;
		PositionPanel(sSavePanel);
		sSavePanel->Show();
		return 0;
	}
	delete sOpenPanel;
	sOpenPanel = new BFilePanel(B_OPEN_PANEL, &target, NULL,
        save == 2 ? B_DIRECTORY_NODE : B_FILE_NODE | B_DIRECTORY_NODE, save == 3, NULL, NULL, false, true);
	ExtensionFilter* filter = new ExtensionFilter(extensions);
	sOpenPanel->SetRefFilter(filter);
	delete sOpenFilter;
	sOpenFilter = filter;
	BMessage message(B_REFS_RECEIVED);
	message.AddInt32(kDialogField, id);
	sOpenPanel->SetMessage(&message);
	if (name != NULL && name[0] != '\0')
		sOpenPanel->SetPanelDirectory(name);
	if (sOpenPanel->Window()->Lock()) {
		sOpenPanel->Window()->SetTitle(title);
		sOpenPanel->Window()->Unlock();
	}
	sOpenPanel->Refresh();
	sDialogId = id;
	sDialogAnswered = false;
	PositionPanel(sOpenPanel);
	sOpenPanel->Show();
	return 0;
}


int32_t
pc_host_open_url(const char* url)
{
	BUrl target(url, true);
	if (!target.IsValid())
		return -1;
	return target.OpenWithPreferredApplication(false) == B_OK ? 0 : -1;
}




// Makes the application the preferred one for the types (one per line) that
// have none yet, so Tracker opens supported documents with it; a
// preferred application the user chose stays.
int32_t
pc_host_claim_types(const char* signature, const char* types)
{
	BString list(types);
	int32 claimed = 0;
	int32 start = 0;
	while (start < list.Length()) {
		int32 end = list.FindFirst('\n', start);
		if (end < 0)
			end = list.Length();
		BString type;
		list.CopyInto(type, start, end - start);
		start = end + 1;
		BMimeType mime(type.String());
		if (mime.InitCheck() != B_OK)
			continue;
		char preferred[B_MIME_TYPE_LENGTH];
		if (mime.IsInstalled() && mime.GetPreferredApp(preferred) == B_OK
			&& preferred[0] != '\0')
			continue;
		if (!mime.IsInstalled())
			mime.Install();
		if (mime.SetPreferredApp(signature) == B_OK)
			claimed++;
	}
	return claimed;
}


// Gives a file the system saved for us its MIME type (by extension and
// content), as files saved by Haiku programs have, so Tracker opens it with
// the right application. Leaves a type already set alone.
int32_t
pc_host_type_file(const char* path)
{
	return update_mime_info(path, false, true, B_UPDATE_MIME_INFO_NO_FORCE)
		>= B_OK ? 0 : -1;
}


void
pc_host_alert(const char* title, const char* text)
{
	BAlert* alert = new BAlert(title, text, "OK", NULL, NULL,
		B_WIDTH_AS_USUAL, B_STOP_ALERT);
	alert->Go();
}


// #pragma mark - Native menu bar, screen picker, displays


// Replaces the main window's menu bar with one built from a spec
// (BuildMenuBar); an empty spec removes it.
void
pc_menu_build(const char* spec)
{
	PcWindow* main = LockMainWindow();
	if (main == NULL)
		return;
	main->SetMenuBar(spec != NULL && spec[0] != '\0'
		? BuildMenuBar(spec) : NULL);
	if (spec == NULL || spec[0] == '\0')
		sMenuItems.clear();
	main->Unlock();
}


// Updates items of the menu bar in place, one per line: index, label,
// enabled (0/1) and checked (-1, 0, 1), tab-separated.
void
pc_menu_update(const char* spec)
{
	PcWindow* main = LockMainWindow();
	if (main == NULL)
		return;
	BString text(spec != NULL ? spec : "");
	int32 start = 0;
	while (start < text.Length()) {
		int32 end = text.FindFirst('\n', start);
		if (end < 0)
			end = text.Length();
		BString line;
		text.CopyInto(line, start, end - start);
		start = end + 1;
		std::vector<BString> f = SplitTabs(line);
		if (f.size() < 4)
			continue;
		int32 index = atoi(f[0].String());
		if (index < 0 || (size_t)index >= sMenuItems.size()
			|| sMenuItems[index] == NULL)
			continue;
		BMenuItem* item = sMenuItems[index];
		if (f[1] != item->Label())
			item->SetLabel(f[1].String());
		bool enabled = atoi(f[2].String()) != 0;
		if (item->IsEnabled() != enabled)
			item->SetEnabled(enabled);
		bool marked = atoi(f[3].String()) > 0;
		if (item->IsMarked() != marked)
			item->SetMarked(marked);
	}
	main->Unlock();
}


// Shows the screen colour picker over a snapshot of the whole screen. Its
// answer arrives as a PC_SCREEN_PICK event with the id. -1 when the screen
// couldn't be read.
int32_t
pc_host_screen_pick(int32_t id, const char* hint, const char* wheelHint)
{
	BScreen screen(B_MAIN_SCREEN_ID);
	if (!screen.IsValid())
		return -1;
	BBitmap* shot = NULL;
	if (screen.GetBitmap(&shot, false) != B_OK || shot == NULL)
		return -1;
	if (shot->ColorSpace() != B_RGB32 && shot->ColorSpace() != B_RGBA32) {
		BBitmap* converted = new BBitmap(shot->Bounds(), B_RGB32);
		status_t status = converted->InitCheck();
		if (status == B_OK)
			status = converted->ImportBits(shot);
		delete shot;
		if (status != B_OK) {
			delete converted;
			return -1;
		}
		shot = converted;
	}
	{
		std::lock_guard<std::mutex> lock(sPickLock);
		if (sPicker.IsValid())
			sPicker.SendMessage(B_QUIT_REQUESTED);
		sPickId = id;
		sPickAnswered = false;
	}
	PickerWindow* window = new PickerWindow(screen.Frame(), shot, id,
		hint != NULL ? hint : "", wheelHint != NULL ? wheelHint : "");
	sPicker = BMessenger(window);
	window->Show();
	window->Activate(true);
	return 0;
}


// Closes the picker of that pick, if it is still up (the interface stopped
// waiting for it).
void
pc_host_screen_pick_cancel(int32_t id)
{
	std::lock_guard<std::mutex> lock(sPickLock);
	if (id == sPickId && sPicker.IsValid())
		sPicker.SendMessage(B_QUIT_REQUESTED);
}


// Where the pointer is, in the main window's canvas view's coordinates
// (logical pixels, outside the view too). 0 when there is no window.
int32_t
pc_host_cursor(float* x, float* y)
{
	PcWindow* main = LockMainWindow();
	if (main == NULL)
		return 0;
	BPoint where;
	uint32 buttons;
	main->View()->GetMouse(&where, &buttons, false);
	main->Unlock();
	*x = where.x;
	*y = where.y;
	return 1;
}


void
pc_host_beep()
{
	beep();
}


// The displays, one per line, tab-separated: id, frame (left, top, right,
// bottom, as window frames are measured), primary (0/1), connector, monitor
// name, vendor, serial number. air/OS: every enabled display of its layout
// that isn't a mirror; elsewhere the screen, with BScreen::GetMonitorInfo's
// name and serial number.
static BString
Field(const char* text)
{
	BString field(text != NULL ? text : "");
	field.ReplaceAll('\t', ' ');
	field.ReplaceAll('\n', ' ');
	return field.Trim();
}


char*
pc_host_displays()
{
	BString lines;
	typedef status_t (*GetLayout)(BMessage&);
	static GetLayout getLayout = (GetLayout)dlsym(RTLD_DEFAULT,
		"_ZN8BPrivate18get_display_layoutER8BMessage");
	BMessage layout;
	if (getLayout != NULL && getLayout(layout) == B_OK) {
		BMessage display;
		for (int32 i = 0; layout.FindMessage("display", i, &display) == B_OK;
				i++) {
			BRect frame;
			if (!display.GetBool("connected", true)
				|| !display.GetBool("enabled", true)
				|| display.GetInt32("mirror", -1) >= 0
				|| display.FindRect("frame", &frame) != B_OK
				|| !frame.IsValid())
				continue;
			BString line;
			line.SetToFormat("%" B_PRId32 "\t%g\t%g\t%g\t%g\t%d\t%s\t%s\t%s\t%s\n",
				display.GetInt32("id", i), frame.left, frame.top,
				frame.right + 1, frame.bottom + 1,
				display.GetBool("primary", false) ? 1 : 0,
				Field(display.GetString("name", "")).String(),
				Field(display.GetString("monitor", "")).String(),
				Field(display.GetString("vendor", "")).String(),
				Field(display.GetString("serial", "")).String());
			lines << line;
		}
	}
	if (lines.Length() == 0) {
		BScreen screen(B_MAIN_SCREEN_ID);
		monitor_info info;
		memset(&info, 0, sizeof(info));
		if (screen.IsValid() && screen.GetMonitorInfo(&info) != B_OK)
			memset(&info, 0, sizeof(info));
		BRect frame = screen.Frame();
		if (screen.IsValid()) {
			lines.SetToFormat("%" B_PRId32 "\t%g\t%g\t%g\t%g\t1\t\t%s\t%s\t%s\n",
				screen.ID().id, frame.left, frame.top, frame.right + 1,
				frame.bottom + 1, Field(info.name).String(),
				Field(info.vendor).String(), Field(info.serial_number).String());
		}
	}
	return strdup(lines.String());
}


}
