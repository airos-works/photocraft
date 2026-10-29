//! Pen tablets on air/OS: pressure, tilt and the eraser end from the mouse messages Haiku's tablet
//! input add-ons send (`src/add-ons/input_server/devices/{tablet,wacom,easypen}`), written into
//! the UI's `StylusFeed` like the desktop app's tablet readers (`apps/photocraft/src/tablet.rs`).
//!
//! The add-ons put the pen's state in `B_MOUSE_MOVED`: `be:tablet_pressure` (0..1),
//! `be:tablet_eraser`, and `be:tablet_tilt_x`/`_y` when the pen reports tilt. They send moves
//! only while the pen is in range, at pressure 0 while it hovers. The HID tablet add-on's
//! `B_MOUSE_DOWN` and `B_MOUSE_UP` carry no sample, only `be:device_subtype`
//! `B_TABLET_POINTING_DEVICE`: the pen keeps its last sample. A message without either is a
//! mouse's (or a touchpad's), which paints at full pressure.
//!
//! Tilt has two scales. The HID add-on (which sends `be:device_subtype`) passes the HID report's
//! value scaled to 0..1 over its logical range, upright at 0.5; the Wacom add-on (which doesn't)
//! sends -1..1, from Wacom's ±64 raw steps of about a degree each. Both become ±64° here, the
//! range of the pens the add-ons know; Pointer Events' convention (-90..90, positive to the right
//! and towards the user) is the UI's.

use photocraft_ui_egui::stylus::PenSample;

/// `B_TABLET_POINTING_DEVICE` (`<Input.h>`).
const TABLET: i32 = 3;

/// Degrees of tilt at the end of the add-ons' range.
const TILT_DEGREES: f32 = 64.0;

/// What a mouse message says about the pen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Update {
    /// A pen sample.
    Pen(PenSample),
    /// A tablet's message without a sample: the pen keeps its last one.
    Keep,
    /// Not a pen: a mouse paints at full pressure.
    Mouse,
}

/// The fields of a mouse event as the host reads them (`ReadTablet` in host.cpp).
#[derive(Clone, Copy, Debug, Default)]
pub struct Raw {
    /// 1 with a sample, 2 for a tablet's message without one, 0 otherwise.
    pub tablet: i32,
    pub pressure: f32,
    pub tilt: Option<(f32, f32)>,
    pub eraser: bool,
    /// `be:device_subtype`, -1 when the message had none.
    pub subtype: i32,
}

/// The pressure a pen touches with when its message doesn't say: the HID add-on's
/// `B_MOUSE_DOWN` comes when the tip switch closes, at the pen's lightest pressure.
pub const CONTACT_PRESSURE: f32 = 1.0 / 256.0;

/// The sample for a tablet's `B_MOUSE_DOWN` without one: the pen's last sample, but touching
/// (the last one is the hover's, at pressure 0, which the canvas reads as no contact: the press
/// then took the pressure of the stroke's later samples and began with a blob).
pub fn contact(last: Option<PenSample>) -> Option<PenSample> {
    let last = last?;
    (last.pressure <= 0.0).then_some(PenSample { pressure: CONTACT_PRESSURE, ..last })
}

pub fn update(raw: Raw) -> Update {
    match raw.tablet {
        1 => {
            let (tilt_x, tilt_y) = raw.tilt.map_or((0.0, 0.0), |(x, y)| {
                let unit = |v: f32| if raw.subtype == TABLET { (v - 0.5) * 2.0 } else { v };
                (unit(x) * TILT_DEGREES, unit(y) * TILT_DEGREES)
            });
            Update::Pen(PenSample { pressure: raw.pressure, tilt_x, tilt_y, rotation: 0.0, eraser: raw.eraser }.sanitized())
        }
        2 => Update::Keep,
        _ => Update::Mouse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_tablet_samples_carry_pressure_and_centred_tilt() {
        let raw = Raw { tablet: 1, pressure: 0.4, tilt: Some((0.75, 0.5)), eraser: false, subtype: TABLET };
        let Update::Pen(s) = update(raw) else { panic!("a pen sample") };
        assert_eq!(s.pressure, 0.4);
        assert_eq!(s.tilt_x, 32.0);
        assert_eq!(s.tilt_y, 0.0);
        assert!(!s.eraser);
    }

    #[test]
    fn wacom_tilt_is_signed_and_the_eraser_end_is_reported() {
        // The Wacom add-on sends no be:device_subtype.
        let raw = Raw { tablet: 1, pressure: 1.0, tilt: Some((-0.5, 1.0)), eraser: true, subtype: -1 };
        let Update::Pen(s) = update(raw) else { panic!("a pen sample") };
        assert_eq!((s.tilt_x, s.tilt_y), (-32.0, 64.0));
        assert!(s.eraser);
    }

    #[test]
    fn hovering_pen_is_pressure_zero_without_tilt() {
        let raw = Raw { tablet: 1, pressure: 0.0, tilt: None, eraser: false, subtype: TABLET };
        assert_eq!(update(raw), Update::Pen(PenSample { pressure: 0.0, ..Default::default() }));
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let raw = Raw { tablet: 1, pressure: 3.0, tilt: Some((9.0, -9.0)), eraser: false, subtype: -1 };
        let Update::Pen(s) = update(raw) else { panic!("a pen sample") };
        assert_eq!((s.pressure, s.tilt_x, s.tilt_y), (1.0, 90.0, -90.0));
    }

    #[test]
    fn a_touch_from_hover_starts_at_the_lightest_pressure() {
        let hover = PenSample { pressure: 0.0, tilt_x: 10.0, eraser: true, ..Default::default() };
        assert_eq!(contact(Some(hover)), Some(PenSample { pressure: CONTACT_PRESSURE, ..hover }));
        // Already pressing (a button pressed during the stroke), or no pen: nothing to change.
        assert_eq!(contact(Some(PenSample { pressure: 0.4, ..Default::default() })), None);
        assert_eq!(contact(None), None);
    }

    #[test]
    fn tablet_button_messages_keep_the_sample_and_mice_reset_it() {
        assert_eq!(update(Raw { tablet: 2, subtype: TABLET, ..Default::default() }), Update::Keep);
        assert_eq!(update(Raw { tablet: 0, subtype: 1, ..Default::default() }), Update::Mouse);
        assert_eq!(update(Raw { tablet: 0, subtype: -1, ..Default::default() }), Update::Mouse);
    }
}
