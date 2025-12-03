//! Input conversion utilities.
//!
//! This module provides a converter from `winit` keyboard events (0.30) to `termwiz`'s `KeyCode`
//! (as used by `wezterm-term`).

use winit::{
    event::{self, DeviceId, ElementState, KeyEvent, MouseScrollDelta, TouchPhase},
    keyboard::{Key, ModifiersState, NamedKey},
};

use termwiz::input::{KeyCode, Modifiers};
use wezterm_term::{MouseButton, MouseEventKind};

use massive_applications::ViewEvent;
use massive_geometry::Point;
use massive_input::Event;

/// Convert a full `winit` `KeyEvent` to a `(KeyCode, Modifiers)` pair.
/// Returns `None` when the event shouldn't be forwarded to the terminal
/// (e.g. pure modifier changes or unsupported keys).
pub fn convert_key_event(event: &KeyEvent, mods: ModifiersState) -> Option<(KeyCode, Modifiers)> {
    let keycode = convert_key(&event.logical_key)?;
    let converted_mods = convert_modifiers(mods);
    Some((keycode, converted_mods))
}

/// Convert a `winit` `ModifiersState` to `termwiz` `Modifiers`.
pub fn convert_modifiers(mods: ModifiersState) -> Modifiers {
    let mut out = Modifiers::NONE;
    if mods.shift_key() {
        out |= Modifiers::SHIFT;
    }
    if mods.control_key() {
        out |= Modifiers::CTRL;
    }
    if mods.alt_key() {
        out |= Modifiers::ALT;
    }
    if mods.super_key() {
        out |= Modifiers::SUPER;
    }
    out
}

/// Convert a `winit` logical `Key` to a `termwiz` `KeyCode`.
/// Returns `None` if the key should be ignored (e.g. pure modifier).
fn convert_key(key: &Key) -> Option<KeyCode> {
    match key {
        Key::Character(s) => {
            // Return None for empty strings; take first char if multi-char (IME preedit may send more).
            let ch = s.chars().next()?;
            Some(KeyCode::Char(ch))
        }
        Key::Named(named) => convert_named_key(named),
        // Dead keys and other variants may appear; skip them for now.
        _ => None,
    }
}

fn convert_named_key(named: &NamedKey) -> Option<KeyCode> {
    match named {
        NamedKey::CapsLock => Some(KeyCode::CapsLock),
        NamedKey::NumLock => Some(KeyCode::NumLock),
        NamedKey::ScrollLock => Some(KeyCode::ScrollLock),
        NamedKey::Enter => Some(KeyCode::Enter),
        NamedKey::Tab => Some(KeyCode::Tab),
        NamedKey::Space => Some(KeyCode::Char(' ')),
        NamedKey::Backspace => Some(KeyCode::Backspace),
        NamedKey::Escape => Some(KeyCode::Escape),
        NamedKey::Delete => Some(KeyCode::Delete),
        NamedKey::Insert => Some(KeyCode::Insert),
        NamedKey::Home => Some(KeyCode::Home),
        NamedKey::End => Some(KeyCode::End),
        NamedKey::PageUp => Some(KeyCode::PageUp),
        NamedKey::PageDown => Some(KeyCode::PageDown),
        NamedKey::ArrowUp => Some(KeyCode::UpArrow),
        NamedKey::ArrowDown => Some(KeyCode::DownArrow),
        NamedKey::ArrowLeft => Some(KeyCode::LeftArrow),
        NamedKey::ArrowRight => Some(KeyCode::RightArrow),
        NamedKey::Clear => Some(KeyCode::Clear),
        NamedKey::Select => Some(KeyCode::Select),
        NamedKey::Print => Some(KeyCode::Print),
        NamedKey::Execute => Some(KeyCode::Execute),
        NamedKey::Help => Some(KeyCode::Help),
        NamedKey::Cancel => Some(KeyCode::Cancel),
        NamedKey::Copy => Some(KeyCode::Copy),
        NamedKey::Cut => Some(KeyCode::Cut),
        NamedKey::Paste => Some(KeyCode::Paste),
        NamedKey::F1 => Some(KeyCode::Function(1)),
        NamedKey::F2 => Some(KeyCode::Function(2)),
        NamedKey::F3 => Some(KeyCode::Function(3)),
        NamedKey::F4 => Some(KeyCode::Function(4)),
        NamedKey::F5 => Some(KeyCode::Function(5)),
        NamedKey::F6 => Some(KeyCode::Function(6)),
        NamedKey::F7 => Some(KeyCode::Function(7)),
        NamedKey::F8 => Some(KeyCode::Function(8)),
        NamedKey::F9 => Some(KeyCode::Function(9)),
        NamedKey::F10 => Some(KeyCode::Function(10)),
        NamedKey::F11 => Some(KeyCode::Function(11)),
        NamedKey::F12 => Some(KeyCode::Function(12)),
        NamedKey::F13 => Some(KeyCode::Function(13)),
        NamedKey::F14 => Some(KeyCode::Function(14)),
        NamedKey::F15 => Some(KeyCode::Function(15)),
        NamedKey::F16 => Some(KeyCode::Function(16)),
        NamedKey::F17 => Some(KeyCode::Function(17)),
        NamedKey::F18 => Some(KeyCode::Function(18)),
        NamedKey::F19 => Some(KeyCode::Function(19)),
        NamedKey::F20 => Some(KeyCode::Function(20)),
        NamedKey::F21 => Some(KeyCode::Function(21)),
        NamedKey::F22 => Some(KeyCode::Function(22)),
        NamedKey::F23 => Some(KeyCode::Function(23)),
        NamedKey::F24 => Some(KeyCode::Function(24)),
        NamedKey::BrowserBack => Some(KeyCode::BrowserBack),
        NamedKey::BrowserFavorites => Some(KeyCode::BrowserFavorites),
        NamedKey::BrowserForward => Some(KeyCode::BrowserForward),
        NamedKey::BrowserHome => Some(KeyCode::BrowserHome),
        NamedKey::BrowserRefresh => Some(KeyCode::BrowserRefresh),
        NamedKey::BrowserSearch => Some(KeyCode::BrowserSearch),
        NamedKey::BrowserStop => Some(KeyCode::BrowserStop),
        NamedKey::AudioVolumeMute => Some(KeyCode::VolumeMute),
        NamedKey::AudioVolumeDown => Some(KeyCode::VolumeDown),
        NamedKey::AudioVolumeUp => Some(KeyCode::VolumeUp),
        NamedKey::MediaTrackNext => Some(KeyCode::MediaNextTrack),
        NamedKey::MediaTrackPrevious => Some(KeyCode::MediaPrevTrack),
        NamedKey::MediaStop => Some(KeyCode::MediaStop),
        NamedKey::MediaPlayPause => Some(KeyCode::MediaPlayPause),
        NamedKey::PrintScreen => Some(KeyCode::PrintScreen),
        NamedKey::Pause => Some(KeyCode::Pause),
        NamedKey::ContextMenu => Some(KeyCode::Menu),
        // All remaining keys not explicitly mapped above are ignored for now.
        _ => None,
    }
}

/// Convert an `Event<ViewEvent>` to mouse event data for terminal forwarding.
/// Returns `(MouseEventKind, MouseButton, Point)` if the event is a mouse event.
pub fn convert_mouse_event_from_view(
    ev: &Event<ViewEvent>,
) -> Option<(MouseEventKind, MouseButton, Point)> {
    let view_event = ev.event();
    let pos = ev.pos()?;

    let (kind, button) = match view_event {
        ViewEvent::CursorMoved { device_id, .. } => (
            MouseEventKind::Move,
            mouse_button_pressed_on_device(ev, *device_id).unwrap_or(MouseButton::None),
        ),
        ViewEvent::MouseWheel {
            delta: MouseScrollDelta::LineDelta(xd, 0.0),
            phase: TouchPhase::Moved,
            ..
        } => (
            MouseEventKind::Press,
            if *xd < 0.0 {
                MouseButton::WheelLeft((-xd).round() as usize)
            } else {
                MouseButton::WheelRight(xd.round() as usize)
            },
        ),
        ViewEvent::MouseWheel {
            delta: MouseScrollDelta::LineDelta(0.0, yd),
            phase: TouchPhase::Moved,
            ..
        } => (
            MouseEventKind::Press,
            if *yd < 0.0 {
                MouseButton::WheelUp((-yd).round() as usize)
            } else {
                MouseButton::WheelDown(yd.round() as usize)
            },
        ),
        ViewEvent::MouseInput { state, button, .. } => (
            match state {
                ElementState::Pressed => MouseEventKind::Press,
                ElementState::Released => MouseEventKind::Release,
            },
            convert_mouse_button(*button)?,
        ),
        _ => return None,
    };

    Some((kind, button, pos))
}

fn mouse_button_pressed_on_device(
    ev: &Event<ViewEvent>,
    device_id: DeviceId,
) -> Option<MouseButton> {
    let (button, _) = ev
        .states()
        .pointing_device(device_id)?
        .buttons
        .iter()
        .filter(|(_, s)| s.element == ElementState::Pressed)
        // ADR: Deciding to return the latest pressed one.
        .max_by_key(|(_, s)| s.when)?;

    convert_mouse_button(*button)
}

fn convert_mouse_button(button: event::MouseButton) -> Option<MouseButton> {
    match button {
        event::MouseButton::Left => Some(MouseButton::Left),
        event::MouseButton::Right => Some(MouseButton::Right),
        event::MouseButton::Middle => Some(MouseButton::Middle),
        event::MouseButton::Back => None,
        event::MouseButton::Forward => None,
        event::MouseButton::Other(_) => None,
    }
}
