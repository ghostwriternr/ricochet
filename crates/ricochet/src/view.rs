use rune::{Cx, El, TextStyle, col, row, spacer, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, Progress};

use crate::{Leg, Side, State, Step};

const HEADLINE: TextStyle = TextStyle::new(20.0, 600.0);

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    let k = rune_theme_opencode::kit(cx.system_dark());
    let refresh = Button::new("Refresh")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Regular)
        .disabled(s.import.busy() && s.push.busy())
        .on_press(|s: &mut State| s.refresh())
        .build(k)
        .key("refresh");
    let header = row()
        .center_y()
        .child(text("Ricochet").font(HEADLINE).color(k.c.text_strong))
        .child(spacer())
        .child(refresh);
    let library = match s.library {
        Some((files, bytes)) => format!("~/Pictures/GR IV · {} files · {}", thousands(files), size(bytes)),
        None => "~/Pictures/GR IV".into(),
    };
    col()
        .w_full()
        .h_full()
        .padding(20.0)
        .gap(12.0)
        .fill(k.c.bg_base)
        .child(header)
        .child(step(k, Side::Import, &s.import))
        .child(step(k, Side::Push, &s.push))
        .child(spacer())
        .child(text(library).key("library").font(k.ty.text_small).color(k.c.text_weak).truncate())
}

fn step(k: &Kit, side: Side, step: &Step) -> El {
    let (route, verb, device_kind) = match side {
        Side::Import => ("Camera → Laptop", "Import", "camera"),
        Side::Push => ("Laptop → iPhone", "Send", "iPhone"),
    };
    let key = match side {
        Side::Import => "import",
        Side::Push => "push",
    };
    let device = match &step.leg {
        Leg::Ready { device, .. } | Leg::Running { device, .. } => shown_name(device, device_kind),
        Leg::Absent => format!("No {device_kind}"),
        Leg::Checking | Leg::Failed { .. } => String::new(),
    };
    let (headline, detail) = match &step.leg {
        Leg::Checking => ("Checking…".to_owned(), String::new()),
        Leg::Absent => match side {
            Side::Import => ("Connect your camera".into(), "Plug it in over USB and switch it on.".into()),
            Side::Push => ("Connect your iPhone".into(), "Plug it in, unlock it, and trust this computer.".into()),
        },
        Leg::Ready { files: 0, .. } => ("Up to date".into(), step.last.clone().unwrap_or_default()),
        Leg::Ready { files, bytes, raw, jpg, .. } => (
            format!("{} new {}", thousands(*files), if *files == 1 { "file" } else { "files" }),
            format!("{} · {raw} RAW, {jpg} JPG", size(*bytes)),
        ),
        Leg::Running { index, files, name, done, bytes, .. } => (
            format!("{} {} of {}", if side == Side::Import { "Importing" } else { "Sending" }, index + 1, files),
            format!("{name} · {} of {}", size(*done), size(*bytes)),
        ),
        Leg::Failed { error } => ("Couldn't finish".into(), error.clone()),
    };
    let action = match &step.leg {
        Leg::Ready { files, .. } if *files > 0 => Some(
            Button::new(verb)
                .variant(ButtonVariant::Submit)
                .on_press(move |s: &mut State| s.start(side, true))
                .build(k),
        ),
        Leg::Running { .. } => Some(
            Button::new("Stop")
                .variant(ButtonVariant::Outline)
                .on_press(move |s: &mut State| s.cancel(side))
                .build(k),
        ),
        Leg::Failed { .. } => Some(
            Button::new("Try again")
                .variant(ButtonVariant::Outline)
                .on_press(move |s: &mut State| s.start(side, false))
                .build(k),
        ),
        _ => None,
    };
    let top = row()
        .center_y()
        .gap(8.0)
        .child(text(route).font(k.ty.text_small_medium).color(k.c.text_weak))
        .child(spacer())
        .child(text(device).key("device").font(k.ty.text_small).color(k.c.text_weak).truncate());
    let detail_color = if matches!(step.leg, Leg::Failed { .. }) { k.c.danger.fg } else { k.c.text_weak };
    let body = col()
        .gap(2.0)
        .grow(1.0)
        .min_w(0.0)
        .child(text(headline).key("headline").font(k.ty.body_medium).color(k.c.text_strong))
        .child(text(detail).key("detail").font(k.ty.text_small).color(detail_color).truncate_middle());
    let mut middle = row().center_y().gap(12.0).child(body);
    if let Some(action) = action {
        middle = middle.child(action.key("action"));
    }
    let mut card = col()
        .key(key)
        .w_full()
        .padding(16.0)
        .gap(10.0)
        .radius(k.radius.md)
        .fill(k.c.surface_raised_base)
        .border(1.0, k.c.border_weak)
        .child(top)
        .child(middle);
    if let Leg::Running { done, bytes, .. } = step.leg {
        card = card.child(Progress::new(Some(done as f32 / bytes.max(1) as f32)).build(k).key("progress"));
    }
    card
}

/// A device's name with only letters, digits, spaces and common punctuation. Emoji go because
/// Rune doesn't draw Linux's bitmap emoji font yet: a phone named "👻" showed an empty box.
/// A name with nothing left is its kind ("iPhone").
fn shown_name(name: &str, kind: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || "'’-_.,()&+".contains(*c))
        .collect();
    let kept = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if kept.is_empty() { kind.to_owned() } else { kept }
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.2} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.0} MB", b as f64 / 1e6),
        b => format!("{:.0} KB", b as f64 / 1e3),
    }
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::shown_name;

    #[test]
    fn names_lose_emoji_and_fall_back_to_the_kind() {
        assert_eq!(shown_name("👻", "iPhone"), "iPhone");
        assert_eq!(shown_name("Naresh’s iPhone 🚀", "iPhone"), "Naresh’s iPhone");
        assert_eq!(shown_name("RICOH GR IV", "camera"), "RICOH GR IV");
    }
}
