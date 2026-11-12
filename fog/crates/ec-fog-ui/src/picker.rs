// SPDX-License-Identifier: AGPL-3.0-only

//! Picker mode's window state (ADR 0100, FOG §File picker): what the bottom
//! bar holds (name, filter, choices, the open menu, the overwrite question)
//! and how a confirm becomes a [`pick::Answer`] in each mode. Pure state: the
//! view draws it, `app` feeds it keys, [`pick::confirm`] writes it. No I/O;
//! whether a save target exists is read from the listing `ec-fogd` sent.

use ec_fog_proto::Kind;

use crate::edit::LineEdit;
use crate::pick::{Answer, ChoiceValue, PickMode, PickRequest, Refused};
use crate::state::{join, Browser};

/// Which dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Of {
    Filter,
    /// Index into [`PickRequest::choices`] (a combo).
    Choice(usize),
}

/// An open dropdown: `at` is the keyboard's row in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Menu {
    pub of: Of,
    pub at: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ui {
    /// `save`: the name being typed.
    pub name: LineEdit,
    /// The name field has the keyboard (and the gold caret).
    pub typing: bool,
    /// The filter in use, an index into the request's filters.
    pub filter: Option<usize>,
    /// One value per request choice.
    pub choices: Vec<ChoiceValue>,
    pub menu: Option<Menu>,
    /// `save`: the target that exists and awaits a replace answer.
    pub replace: Option<Vec<u8>>,
    /// The folder in which the user last moved the cursor or clicked. The
    /// cursor always rests on a row; it only counts as a choice once the
    /// user has put it there.
    pub touched: Option<Vec<u8>>,
    /// Why the last accept was refused.
    pub hint: Option<&'static str>,
}

impl Ui {
    pub fn new(req: &PickRequest) -> Self {
        let name = req.current_name.clone().unwrap_or_default();
        // The caret sits before the extension, where a rename would start.
        let stem = match name.rfind('.') {
            Some(i) if i > 0 => i,
            _ => name.len(),
        };
        let filter = match (req.current_filter, req.filters.len()) {
            (Some(i), n) if i < n => Some(i),
            (_, 0) => None,
            _ => Some(0),
        };
        Self {
            name: LineEdit::at(name, stem),
            typing: req.mode.named(),
            filter,
            choices: req.choices.iter().map(|c| c.default).collect(),
            menu: None,
            replace: None,
            touched: None,
            hint: None,
        }
    }

    /// The globs of the filter in use. Mime types are not filtered yet:
    /// `ec-fogd`'s listing carries no mime, so a filter of mimes alone shows
    /// every file.
    pub fn globs(&self, req: &PickRequest) -> Vec<String> {
        self.filter
            .and_then(|i| req.filters.get(i))
            .map(|f| f.globs.clone())
            .unwrap_or_default()
    }

    /// The rows of dropdown `of`.
    pub fn options<'a>(&self, req: &'a PickRequest, of: Of) -> Vec<&'a str> {
        match of {
            Of::Filter => req.filters.iter().map(|f| f.name.as_str()).collect(),
            Of::Choice(i) => req
                .choices
                .get(i)
                .map(|c| c.options.iter().map(String::as_str).collect())
                .unwrap_or_default(),
        }
    }

    /// The row of `of` in use.
    pub fn current(&self, of: Of) -> usize {
        match of {
            Of::Filter => self.filter.unwrap_or(0),
            Of::Choice(i) => match self.choices.get(i) {
                Some(ChoiceValue::Option(n)) => *n,
                _ => 0,
            },
        }
    }

    /// Set dropdown `of` to row `n`. `true` if that is the filter, whose mask
    /// the caller must re-apply.
    pub fn set(&mut self, of: Of, n: usize) -> bool {
        match of {
            Of::Filter => {
                self.filter = Some(n);
                true
            }
            Of::Choice(i) => {
                if let Some(v) = self.choices.get_mut(i) {
                    *v = ChoiceValue::Option(n);
                }
                false
            }
        }
    }

    pub fn toggle(&mut self, i: usize) {
        if let Some(ChoiceValue::Bool(b)) = self.choices.get_mut(i) {
            *b = !*b;
        }
    }

    /// The typed name, if it can be one.
    pub fn typed_name(&self) -> Option<&str> {
        let n = self.name.text();
        let ok = !n.trim().is_empty() && !n.contains('/') && n != "." && n != "..";
        ok.then_some(n)
    }

    /// What the user has chosen in `b`: the marked rows, else the cursor's
    /// row once the user has put it there.
    pub fn chosen<'a>(&self, b: &'a Browser) -> Vec<&'a ec_fog_proto::Entry> {
        let marked = b.selection();
        if !marked.is_empty() {
            return marked;
        }
        if self.touched.as_deref() == Some(b.path.as_slice()) {
            return b.selected_entry().into_iter().collect();
        }
        Vec::new()
    }

    /// The paths a confirm now would answer, by mode. `Err` is why not,
    /// which is also when the accept button is disabled.
    pub fn paths(&self, req: &PickRequest, b: &Browser) -> Result<Vec<Vec<u8>>, Refused> {
        let chosen = self.chosen(b);
        let of = |dirs: bool| -> Vec<Vec<u8>> {
            chosen
                .iter()
                .filter(|e| (e.kind == Kind::Dir) == dirs)
                .map(|e| join(&b.path, &e.name))
                .collect()
        };
        match req.mode {
            PickMode::Open => of(false)
                .into_iter()
                .next()
                .map(|p| vec![p])
                .ok_or(Refused::NothingChosen),
            PickMode::OpenMultiple => {
                let v = of(false);
                if v.is_empty() {
                    Err(Refused::NothingChosen)
                } else {
                    Ok(v)
                }
            }
            PickMode::Directory => {
                let v = of(true);
                Ok(vec![v.into_iter().next().unwrap_or_else(|| b.path.clone())])
            }
            PickMode::DirectoryMultiple => {
                let v = of(true);
                Ok(if v.is_empty() {
                    vec![b.path.clone()]
                } else {
                    v
                })
            }
            PickMode::SaveFiles => Ok(vec![b.path.clone()]),
            PickMode::Save => {
                let name = self.typed_name().ok_or(Refused::NoName)?;
                Ok(vec![join(&b.path, name.as_bytes())])
            }
        }
    }

    /// The whole answer: [`Self::paths`], the filter and every choice shown.
    pub fn answer(&self, req: &PickRequest, b: &Browser) -> Result<Answer, Refused> {
        Ok(self.answer_of(self.paths(req, b)?))
    }

    /// `paths` with the filter and choices as they stand.
    pub fn answer_of(&self, paths: Vec<Vec<u8>>) -> Answer {
        Answer {
            paths,
            filter: self.filter,
            choices: self.choices.iter().copied().enumerate().collect(),
        }
    }

    /// `save`: the typed name is a file of the shown folder already.
    pub fn exists(&self, b: &Browser) -> bool {
        self.typed_name()
            .is_some_and(|n| b.entries.iter().any(|e| e.name == n.as_bytes()))
    }

    /// `save`: the typed name is a folder, which cannot be replaced by a file.
    pub fn is_folder(&self, b: &Browser) -> bool {
        self.typed_name().is_some_and(|n| {
            b.entries
                .iter()
                .any(|e| e.name == n.as_bytes() && e.kind == Kind::Dir)
        })
    }
}

/// What the bar says it will answer, for the modes that choose by selection.
pub fn summary(req: &PickRequest, ui: &Ui, b: &Browser) -> String {
    let folder = |p: &[u8]| {
        let name = crate::view::basename(p);
        if name.is_empty() {
            "/".to_owned()
        } else {
            name
        }
    };
    match req.mode {
        PickMode::Open | PickMode::OpenMultiple => match ui.paths(req, b) {
            Ok(p) if p.len() == 1 => crate::view::basename(&p[0]),
            Ok(p) => format!("{} files", p.len()),
            Err(_) => String::new(),
        },
        PickMode::Directory | PickMode::DirectoryMultiple => match ui.paths(req, b) {
            Ok(p) if p.len() == 1 => folder(&p[0]),
            Ok(p) => format!("{} folders", p.len()),
            Err(_) => String::new(),
        },
        PickMode::SaveFiles => format!("into {}", folder(&b.path)),
        PickMode::Save => String::new(),
    }
}

/// The accept button's label: the app's, else by mode.
pub fn accept_label(req: &PickRequest) -> String {
    if let Some(l) = req.accept_label.as_ref().filter(|l| !l.trim().is_empty()) {
        return l.clone();
    }
    match req.mode {
        PickMode::Open | PickMode::OpenMultiple => "Open",
        PickMode::Save | PickMode::SaveFiles => "Save",
        PickMode::Directory | PickMode::DirectoryMultiple => "Select",
    }
    .to_owned()
}

/// The window title: the app's, else by mode.
pub fn title(req: &PickRequest) -> String {
    if let Some(t) = req.title.as_ref().filter(|t| !t.trim().is_empty()) {
        return t.clone();
    }
    match req.mode {
        PickMode::Open => "Open file",
        PickMode::OpenMultiple => "Open files",
        PickMode::Directory => "Select folder",
        PickMode::DirectoryMultiple => "Select folders",
        PickMode::Save => "Save file",
        PickMode::SaveFiles => "Save files",
    }
    .to_owned()
}

/// Shell-style glob of a file name: `*`, `?`, `[set]` and `[!set]` with
/// ranges. ASCII case is ignored, so `*.png` also shows `PHOTO.PNG`.
pub fn glob(pat: &str, name: &[u8]) -> bool {
    let name: Vec<char> = String::from_utf8_lossy(name).chars().collect();
    let pat: Vec<char> = pat.chars().collect();
    fold(&pat, &name)
}

fn same(a: char, b: char) -> bool {
    a.eq_ignore_ascii_case(&b)
}

fn fold(pat: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    // Where the last `*` was, and how much of the name it has eaten.
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        let step = match pat.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
                continue;
            }
            Some('?') => Some(1),
            Some('[') => set(&pat[p..], name[n]),
            Some(&c) if same(c, name[n]) => Some(1),
            _ => None,
        };
        match step {
            Some(w) => {
                // `?` and a literal take one pattern char, a set its width.
                p += if pat[p] == '[' { w } else { 1 };
                n += 1;
            }
            None => match star {
                Some((sp, sn)) => {
                    star = Some((sp, sn + 1));
                    p = sp + 1;
                    n = sn + 1;
                }
                None => return false,
            },
        }
    }
    pat[p..].iter().all(|&c| c == '*')
}

/// `pat` starts at a `[`: the width of the set if `c` is in it.
fn set(pat: &[char], c: char) -> Option<usize> {
    let mut i = 1;
    let neg = matches!(pat.get(i), Some('!' | '^'));
    if neg {
        i += 1;
    }
    let first = i;
    let mut hit = false;
    loop {
        let &x = pat.get(i)?;
        if x == ']' && i > first {
            break;
        }
        if pat.get(i + 1) == Some(&'-') && pat.get(i + 2).is_some_and(|&e| e != ']') {
            let e = pat[i + 2];
            let lo = x.to_ascii_lowercase();
            let hi = e.to_ascii_lowercase();
            let cl = c.to_ascii_lowercase();
            hit |= (lo..=hi).contains(&cl);
            i += 3;
        } else {
            hit |= same(x, c);
            i += 1;
        }
    }
    (hit != neg).then_some(i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pick::{Choice, Filter};
    use ec_fog_proto::{Entry, Reply};

    fn req(mode: PickMode) -> PickRequest {
        PickRequest {
            mode,
            title: None,
            accept_label: None,
            current_folder: None,
            current_name: None,
            filters: Vec::new(),
            current_filter: None,
            choices: Vec::new(),
            files: Vec::new(),
            parent: None,
        }
    }

    fn browser(entries: &[(&str, Kind)]) -> Browser {
        let mut b = Browser::new(b"/w".to_vec());
        let _ = b.on_reply(Reply::DirSnapshot {
            path: b"/w".to_vec(),
            dir: 1,
            generation: 0,
            entries: entries
                .iter()
                .map(|(n, k)| Entry::new(n.as_bytes().to_vec(), *k))
                .collect(),
            order: (0..entries.len() as u32).collect(),
            complete: true,
        });
        b
    }

    fn files() -> Browser {
        browser(&[
            ("docs", Kind::Dir),
            ("a.txt", Kind::File),
            ("b.PNG", Kind::File),
            ("c.txt", Kind::File),
        ])
    }

    fn touch(ui: &mut Ui, b: &Browser) {
        ui.touched = Some(b.path.clone());
    }

    #[test]
    fn globs_match_shell_style_and_ignore_ascii_case() {
        assert!(glob("*.png", b"photo.PNG"));
        assert!(glob("*.[pP][nN][gG]", b"x.png"));
        assert!(glob("a?c", b"abc"));
        assert!(glob("[a-c]*", b"bee"));
        assert!(glob("[!a-c]*", b"dee"));
        assert!(glob("*", b""));
        assert!(!glob("*.png", b"photo.png.bak"));
        assert!(!glob("a?c", b"ac"));
        assert!(!glob("[!a-c]*", b"bee"));
        assert!(glob("*.t*t", b"a.txt"));
    }

    #[test]
    fn a_mask_keeps_folders_and_matching_files() {
        let mut b = files();
        let _ = b.set_mask(vec!["*.txt".into()], false);
        assert_eq!(b.len(), 3);
        let _ = b.set_mask(Vec::new(), true);
        assert_eq!(b.len(), 1);
        let _ = b.set_mask(Vec::new(), false);
        assert_eq!(b.len(), 4);
    }

    #[test]
    fn open_answers_the_chosen_file_and_nothing_untouched() {
        let r = req(PickMode::Open);
        let mut b = files();
        let mut ui = Ui::new(&r);
        assert_eq!(ui.paths(&r, &b), Err(Refused::NothingChosen));
        let _ = b.step(1);
        touch(&mut ui, &b);
        assert_eq!(ui.paths(&r, &b), Ok(vec![b"/w/a.txt".to_vec()]));
        // A folder under the cursor is not a file to open.
        let _ = b.step(-1);
        assert_eq!(ui.paths(&r, &b), Err(Refused::NothingChosen));
    }

    #[test]
    fn open_multiple_answers_the_marked_files_in_order() {
        let r = req(PickMode::OpenMultiple);
        let mut b = files();
        let ui = Ui::new(&r);
        b.click(3, crate::state::Click::Only);
        b.click(1, crate::state::Click::Toggle);
        b.click(3, crate::state::Click::Toggle);
        let got = ui.paths(&r, &b).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.contains(&b"/w/a.txt".to_vec()) && got.contains(&b"/w/c.txt".to_vec()));
    }

    #[test]
    fn directory_modes_fall_back_to_the_current_folder() {
        let b = files();
        for mode in [PickMode::Directory, PickMode::DirectoryMultiple] {
            let r = req(mode);
            let mut ui = Ui::new(&r);
            assert_eq!(ui.paths(&r, &b), Ok(vec![b"/w".to_vec()]));
            // The cursor on a folder, put there by the user, is the choice.
            touch(&mut ui, &b);
            assert_eq!(ui.paths(&r, &b), Ok(vec![b"/w/docs".to_vec()]));
        }
        // Touched on a file: not a folder, so the current one.
        let r = req(PickMode::Directory);
        let mut b = files();
        let mut ui = Ui::new(&r);
        let _ = b.step(1);
        touch(&mut ui, &b);
        assert_eq!(ui.paths(&r, &b), Ok(vec![b"/w".to_vec()]));
    }

    #[test]
    fn save_joins_the_folder_and_the_typed_name() {
        let mut r = req(PickMode::Save);
        r.current_name = Some("report.pdf".into());
        let b = files();
        let mut ui = Ui::new(&r);
        assert_eq!(ui.name.halves(), ("report", ".pdf"));
        assert_eq!(ui.paths(&r, &b), Ok(vec![b"/w/report.pdf".to_vec()]));
        for bad in ["", "  ", "a/b", ".."] {
            ui.name = LineEdit::new(bad);
            assert_eq!(ui.paths(&r, &b), Err(Refused::NoName), "{bad:?}");
        }
    }

    #[test]
    fn save_knows_what_exists_from_the_listing() {
        let r = req(PickMode::Save);
        let b = files();
        let mut ui = Ui::new(&r);
        ui.name = LineEdit::new("a.txt");
        assert!(ui.exists(&b) && !ui.is_folder(&b));
        ui.name = LineEdit::new("docs");
        assert!(ui.exists(&b) && ui.is_folder(&b));
        ui.name = LineEdit::new("new.txt");
        assert!(!ui.exists(&b));
    }

    #[test]
    fn save_files_answers_the_folder_whatever_is_chosen() {
        let mut r = req(PickMode::SaveFiles);
        r.files = vec![b"x".to_vec(), vec![0xff, b'y']];
        let b = files();
        let ui = Ui::new(&r);
        assert_eq!(ui.paths(&r, &b), Ok(vec![b"/w".to_vec()]));
        assert_eq!(summary(&r, &ui, &b), "into w");
    }

    #[test]
    fn the_answer_carries_the_filter_and_every_choice() {
        let mut r = req(PickMode::Open);
        r.filters = vec![
            Filter {
                name: "Text".into(),
                globs: vec!["*.txt".into()],
                mimes: Vec::new(),
            },
            Filter {
                name: "Images".into(),
                globs: vec!["*.png".into()],
                mimes: Vec::new(),
            },
        ];
        r.current_filter = Some(1);
        r.choices = vec![
            Choice {
                label: "Encoding".into(),
                options: vec!["utf-8".into(), "latin1".into()],
                default: ChoiceValue::Option(0),
            },
            Choice {
                label: "Read only".into(),
                options: Vec::new(),
                default: ChoiceValue::Bool(false),
            },
        ];
        let mut b = files();
        let mut ui = Ui::new(&r);
        assert_eq!(ui.globs(&r), vec!["*.png".to_owned()]);
        assert!(ui.set(Of::Filter, 0));
        assert!(!ui.set(Of::Choice(0), 1));
        ui.toggle(1);
        let _ = b.step(1);
        touch(&mut ui, &b);
        let a = ui.answer(&r, &b).unwrap();
        assert_eq!(a.filter, Some(0));
        assert_eq!(
            a.choices,
            vec![(0, ChoiceValue::Option(1)), (1, ChoiceValue::Bool(true))]
        );
        // The request's own encoder accepts it.
        assert!(crate::pick::encode(&r, &a).is_ok());
    }

    #[test]
    fn labels_and_titles_default_by_mode() {
        let mut r = req(PickMode::Directory);
        assert_eq!(accept_label(&r), "Select");
        assert_eq!(title(&r), "Select folder");
        r.accept_label = Some("Pick".into());
        r.title = Some("Where?".into());
        assert_eq!(accept_label(&r), "Pick");
        assert_eq!(title(&r), "Where?");
    }
}
