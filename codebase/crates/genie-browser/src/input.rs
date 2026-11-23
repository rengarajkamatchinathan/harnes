//! CDP input: key definitions for `browser_press_key` and the take-over mapping of
//! `browser.input` events (05 §9, SA-24) to `Input.*` commands.

use genie_protocol::api::browser::InputEvent;
use serde_json::{Value, json};

/// CDP modifier bits.
pub const ALT: u32 = 1;
pub const CTRL: u32 = 2;
pub const META: u32 = 4;
pub const SHIFT: u32 = 8;

/// `(key, code, windowsVirtualKeyCode, text)`.
pub type KeyDef = (String, String, u32, Option<String>);

/// The [`KeyDef`] for a key name.
pub fn key_def(name: &str) -> Option<KeyDef> {
    let named: &[(&str, &str, u32, Option<&str>)] = &[
        ("Enter", "Enter", 13, Some("\r")),
        ("Tab", "Tab", 9, None),
        ("Escape", "Escape", 27, None),
        ("Backspace", "Backspace", 8, None),
        ("Delete", "Delete", 46, None),
        ("Space", "Space", 32, Some(" ")),
        ("ArrowUp", "ArrowUp", 38, None),
        ("ArrowDown", "ArrowDown", 40, None),
        ("ArrowLeft", "ArrowLeft", 37, None),
        ("ArrowRight", "ArrowRight", 39, None),
        ("Home", "Home", 36, None),
        ("End", "End", 35, None),
        ("PageUp", "PageUp", 33, None),
        ("PageDown", "PageDown", 34, None),
        ("F5", "F5", 116, None),
    ];
    let lower = name.to_ascii_lowercase();
    let alias = match lower.as_str() {
        "return" => "enter",
        "esc" => "escape",
        "up" => "arrowup",
        "down" => "arrowdown",
        "left" => "arrowleft",
        "right" => "arrowright",
        " " => "space",
        other => other,
    };
    if let Some((k, c, vk, t)) = named.iter().find(|(k, ..)| k.eq_ignore_ascii_case(alias)) {
        let key = if *k == "Space" { " " } else { k };
        return Some((key.into(), (*c).into(), *vk, t.map(String::from)));
    }
    let mut chars = name.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let (code, vk) = if ch.is_ascii_alphabetic() {
        (
            format!("Key{}", ch.to_ascii_uppercase()),
            u32::from(ch.to_ascii_uppercase()),
        )
    } else if ch.is_ascii_digit() {
        (format!("Digit{ch}"), u32::from(ch))
    } else {
        (String::new(), 0)
    };
    Some((ch.to_string(), code, vk, Some(ch.to_string())))
}

/// Parse `Control+Shift+a` into modifiers + key.
pub fn parse_chord(chord: &str) -> Option<(u32, KeyDef)> {
    let parts: Vec<&str> = if chord == "+" {
        vec!["+"]
    } else {
        chord.split('+').collect()
    };
    let (key, mods) = parts.split_last()?;
    let mut m = 0;
    for p in mods {
        m |= match p.to_ascii_lowercase().as_str() {
            "alt" | "option" => ALT,
            "ctrl" | "control" => CTRL,
            "meta" | "cmd" | "command" | "super" => META,
            "shift" => SHIFT,
            _ => return None,
        };
    }
    Some((m, key_def(key)?))
}

/// `Input.dispatchKeyEvent` params for a key press (down + up). Text is only sent when no
/// command modifier is held.
pub fn key_press(modifiers: u32, def: &KeyDef) -> [Value; 2] {
    let (key, code, vk, text) = def;
    let mut down = json!({
        "type": "keyDown", "key": key, "code": code, "modifiers": modifiers,
        "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk,
    });
    if let Some(t) = text
        && modifiers & (CTRL | META | ALT) == 0
    {
        down["text"] = json!(t);
    } else {
        down["type"] = json!("rawKeyDown");
    }
    let up = json!({
        "type": "keyUp", "key": key, "code": code, "modifiers": modifiers,
        "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk,
    });
    [down, up]
}

/// Map a take-over input event to `(method, params)` (05 §9, SA-24). Coordinates are CSS
/// pixels of the viewport (the client divides canvas pixels by `device_scale`).
pub fn to_cdp(ev: &InputEvent) -> (&'static str, Value) {
    match ev {
        InputEvent::Mouse {
            action,
            x,
            y,
            button,
            click_count,
            modifiers,
        } => {
            // `pressed|released|moved` (02 §6.6); `down|up|move` (DOM names) are accepted too.
            let t = match action.as_str() {
                "pressed" | "down" => "mousePressed",
                "released" | "up" => "mouseReleased",
                _ => "mouseMoved",
            };
            (
                "Input.dispatchMouseEvent",
                json!({
                    "type": t, "x": x, "y": y,
                    "button": button.as_deref().unwrap_or("none"),
                    "clickCount": click_count, "modifiers": modifiers,
                }),
            )
        }
        InputEvent::Key {
            action,
            key,
            code,
            modifiers,
            text,
        } => {
            let def = key_def(key);
            // Enter needs its "\r" text or Chrome never fires keypress (no form submit, no
            // newline); clients send text only for printable keys.
            let command = modifiers & (CTRL | META | ALT) != 0;
            let text = match (text, action.as_str()) {
                (Some(t), _) => Some(t.clone()),
                (None, "down") if !command => def.as_ref().and_then(|d| d.3.clone()),
                _ => None,
            };
            let t = match action.as_str() {
                "up" => "keyUp",
                "char" => "char",
                _ if text.is_some() => "keyDown",
                _ => "rawKeyDown",
            };
            let vk = def.map(|d| d.2).unwrap_or(0);
            let mut p = json!({
                "type": t, "key": key, "code": code, "modifiers": modifiers,
                "windowsVirtualKeyCode": vk,
            });
            if let Some(tx) = text {
                p["unmodifiedText"] = json!(tx);
                p["text"] = json!(tx);
            }
            if action == "down"
                && let Some(cmd) = mac_command(*modifiers, key)
            {
                p["commands"] = json!([cmd]);
            }
            ("Input.dispatchKeyEvent", p)
        }
        InputEvent::Wheel {
            x,
            y,
            delta_x,
            delta_y,
        } => (
            "Input.dispatchMouseEvent",
            json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": delta_x, "deltaY": delta_y}),
        ),
        InputEvent::Ime {
            text,
            selection_start,
            selection_end,
        } => (
            "Input.imeSetComposition",
            json!({"text": text, "selectionStart": selection_start, "selectionEnd": selection_end}),
        ),
        InputEvent::InsertText { text } => ("Input.insertText", json!({"text": text})),
    }
}

/// Cmd shortcuts on macOS: Chrome's renderer gets editing commands from the OS key
/// bindings there, which synthetic key events skip, so they are named explicitly. Ctrl
/// shortcuts (Windows/Linux) are handled by the renderer itself.
fn mac_command(modifiers: u32, key: &str) -> Option<&'static str> {
    if modifiers & META == 0 || modifiers & (CTRL | ALT) != 0 {
        return None;
    }
    let shift = modifiers & SHIFT != 0;
    Some(match (key.to_ascii_lowercase().as_str(), shift) {
        ("a", false) => "selectAll",
        ("c", false) => "copy",
        ("x", false) => "cut",
        ("v", false) => "paste",
        ("z", false) => "undo",
        ("z", true) => "redo",
        ("arrowleft", false) => "moveToBeginningOfLine",
        ("arrowright", false) => "moveToEndOfLine",
        ("arrowleft", true) => "moveToBeginningOfLineAndModifySelection",
        ("arrowright", true) => "moveToEndOfLineAndModifySelection",
        ("arrowup", false) => "moveToBeginningOfDocument",
        ("arrowdown", false) => "moveToEndOfDocument",
        ("backspace", false) => "deleteToBeginningOfLine",
        _ => return None,
    })
}

/// Plain pointer movement doesn't take control away from the agent; everything else does.
pub fn takes_control(ev: &InputEvent) -> bool {
    !matches!(ev, InputEvent::Mouse { action, .. } if action == "moved" || action == "move")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_chords() {
        assert_eq!(key_def("Enter").unwrap().2, 13);
        assert_eq!(key_def("esc").unwrap().0, "Escape");
        assert_eq!(key_def("a").unwrap().1, "KeyA");
        assert_eq!(key_def("Space").unwrap().0, " ");
        assert!(key_def("NotAKey").is_none());
        let (m, d) = parse_chord("Control+Shift+a").unwrap();
        assert_eq!(m, CTRL | SHIFT);
        let [down, up] = key_press(m, &d);
        assert_eq!(down["type"], "rawKeyDown");
        assert!(down.get("text").is_none());
        assert_eq!(up["type"], "keyUp");
        let [down, _] = key_press(0, &key_def("Enter").unwrap());
        assert_eq!(down["text"], "\r");
        assert!(parse_chord("Hyper+a").is_none());
    }

    #[test]
    fn takeover_mapping() {
        let (m, p) = to_cdp(&InputEvent::Mouse {
            action: "pressed".into(),
            x: 10.0,
            y: 20.0,
            button: Some("left".into()),
            click_count: 1,
            modifiers: 0,
        });
        assert_eq!(m, "Input.dispatchMouseEvent");
        assert_eq!(p["type"], "mousePressed");
        assert_eq!(p["button"], "left");
        let (m, p) = to_cdp(&InputEvent::Ime {
            text: "にほ".into(),
            selection_start: 2,
            selection_end: 2,
        });
        assert_eq!(m, "Input.imeSetComposition");
        assert_eq!(p["text"], "にほ");
        let (m, _) = to_cdp(&InputEvent::InsertText { text: "x".into() });
        assert_eq!(m, "Input.insertText");
        let (_, p) = to_cdp(&InputEvent::Wheel {
            x: 1.0,
            y: 2.0,
            delta_x: 0.0,
            delta_y: 120.0,
        });
        assert_eq!(p["type"], "mouseWheel");
        assert!(!takes_control(&InputEvent::Mouse {
            action: "moved".into(),
            x: 0.0,
            y: 0.0,
            button: None,
            click_count: 0,
            modifiers: 0
        }));
        assert!(takes_control(&InputEvent::InsertText { text: "x".into() }));
    }

    #[test]
    fn dom_action_names_and_enter_text() {
        let mouse = |action: &str| {
            to_cdp(&InputEvent::Mouse {
                action: action.into(),
                x: 1.0,
                y: 1.0,
                button: Some("left".into()),
                click_count: 1,
                modifiers: 0,
            })
            .1["type"]
                .clone()
        };
        assert_eq!(mouse("down"), "mousePressed");
        assert_eq!(mouse("up"), "mouseReleased");
        assert_eq!(mouse("move"), "mouseMoved");
        assert!(!takes_control(&InputEvent::Mouse {
            action: "move".into(),
            x: 0.0,
            y: 0.0,
            button: None,
            click_count: 0,
            modifiers: 0
        }));
        let key = |k: &str, m: u32| {
            to_cdp(&InputEvent::Key {
                action: "down".into(),
                key: k.into(),
                code: k.into(),
                modifiers: m,
                text: None,
            })
            .1
        };
        let enter = key("Enter", 0);
        assert_eq!(enter["type"], "keyDown");
        assert_eq!(enter["text"], "\r");
        assert_eq!(key("Enter", CTRL)["type"], "rawKeyDown");
        assert_eq!(key("Backspace", 0)["type"], "rawKeyDown");
        assert!(key("Backspace", 0).get("text").is_none());
        // Cmd shortcuts carry editing commands; Ctrl ones don't.
        assert_eq!(key("a", META)["commands"], json!(["selectAll"]));
        assert_eq!(key("z", META | SHIFT)["commands"], json!(["redo"]));
        assert!(key("a", CTRL).get("commands").is_none());
        assert!(key("q", META).get("commands").is_none());
    }
}
