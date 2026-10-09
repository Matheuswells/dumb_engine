//! Small shared widgets.

pub enum InlineEdit {
    Editing,
    /// Enter pressed or clicked elsewhere.
    Commit,
    /// Escape pressed.
    Cancel,
}

/// Inline rename box. Takes focus once when it appears (selecting the name without its
/// extension), commits on Enter or click-away, cancels on Escape.
///
/// `place` puts it at a fixed rect (asset tiles); otherwise it is added to the layout.
pub fn inline_text_edit(ui: &mut egui::Ui, id: egui::Id, text: &mut String, place: Option<egui::Rect>, width: f32) -> InlineEdit {
    let edit = egui::TextEdit::singleline(text).id(id).desired_width(width);
    let r = match place {
        Some(rect) => ui.put(rect, edit),
        None => ui.add(edit),
    };
    let started_key = id.with("started");
    let started = ui.data(|d| d.get_temp::<bool>(started_key)).unwrap_or(false);
    if !started {
        r.request_focus();
        // Select the name part so typing replaces it but keeps ".mat", ".blend", ...
        let stem_len = text.rfind('.').filter(|i| *i > 0).unwrap_or(text.len());
        let stem_chars = text[..stem_len].chars().count();
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
            state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(stem_chars))));
            state.store(ui.ctx(), id);
        }
        ui.data_mut(|d| d.insert_temp(started_key, true));
        return InlineEdit::Editing;
    }
    let done = |ui: &mut egui::Ui| ui.data_mut(|d| d.remove::<bool>(started_key));
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        done(ui);
        return InlineEdit::Cancel;
    }
    if r.lost_focus() || (!r.has_focus() && ui.input(|i| i.pointer.any_pressed())) {
        done(ui);
        return InlineEdit::Commit;
    }
    InlineEdit::Editing
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Run one headless egui frame with the given events.
    pub fn frame(ctx: &egui::Context, events: Vec<egui::Event>, mut f: impl FnMut(&mut egui::Ui)) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let out = ctx.run_ui(input, |ui| f(ui));
        let mut delta = out.textures_delta;
        delta.clear();
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }
    }

    fn run(events: Vec<Vec<egui::Event>>, initial: &str) -> (Vec<&'static str>, String) {
        let ctx = egui::Context::default();
        let mut text = initial.to_string();
        let mut outcomes = Vec::new();
        for evs in events {
            frame(&ctx, evs, |ui| {
                outcomes.push(match inline_text_edit(ui, egui::Id::new("t"), &mut text, None, 200.0) {
                    InlineEdit::Editing => "editing",
                    InlineEdit::Commit => "commit",
                    InlineEdit::Cancel => "cancel",
                });
            });
        }
        (outcomes, text)
    }

    #[test]
    fn enter_commits_typed_name_keeping_extension() {
        // Frame 1 focuses and selects "red" in "red.mat"; typing replaces it; Enter commits.
        let (o, text) = run(vec![vec![], vec![egui::Event::Text("blue".into())], vec![key(egui::Key::Enter)]], "red.mat");
        assert_eq!(o, ["editing", "editing", "commit"]);
        assert_eq!(text, "blue.mat");
    }

    #[test]
    fn escape_cancels() {
        let (o, _) = run(vec![vec![], vec![egui::Event::Text("x".into())], vec![key(egui::Key::Escape)]], "a.txt");
        assert_eq!(o.last(), Some(&"cancel"));
    }

    #[test]
    fn stays_editing_while_typing() {
        let (o, text) = run(vec![vec![], vec![egui::Event::Text("ab".into())], vec![egui::Event::Text("c".into())], vec![]], "");
        assert_eq!(o, ["editing", "editing", "editing", "editing"]);
        assert_eq!(text, "abc");
    }
}
