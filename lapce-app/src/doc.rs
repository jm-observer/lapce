use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        atomic::{self, AtomicUsize},
        Arc,
    },
};

use floem::kurbo::Rect;
use floem::reactive::Trigger;
use floem::views::editor::lines::Lines;
use floem::{
    ext_event::create_ext_action,
    keyboard::Modifiers,
    peniko::Color,
    reactive::{
        batch, ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
    },
    text::FamilyOwned,
    views::editor::{
        command::{Command, CommandExecuted},
        id::EditorId,
        phantom_text::{PhantomText, PhantomTextKind, PhantomTextLine},
        text::{Document, DocumentPhantom, PreeditData, Styling, SystemClipboard},
        view::ScreenLinesBase,
        EditorStyle,
    },
    ViewId,
};
use floem_editor_core::buffer::rope_text::RopeTextVal;
use itertools::Itertools;
use lapce_core::{
    buffer::{
        diff::{rope_diff, DiffLines},
        rope_text::RopeText,
        Buffer, InvalLines,
    },
    char_buffer::CharBuffer,
    command::EditCommand,
    cursor::{Cursor, CursorAffinity},
    editor::{Action, EditConf, EditType},
    indent::IndentStyle,
    language::LapceLanguage,
    line_ending::LineEnding,
    mode::MotionMode,
    register::Register,
    rope_text_pos::RopeTextPosition,
    selection::{InsertDrift, Selection},
    style::line_styles,
    syntax::{edit::SyntaxEdit, BracketParser, Syntax},
    word::{get_char_property, CharClassification, WordCursor},
};
use lapce_rpc::{
    buffer::BufferId,
    plugin::PluginId,
    proxy::ProxyResponse,
    style::{LineStyle, LineStyles, Style},
};
use lapce_xi_rope::{
    spans::{Spans, SpansBuilder},
    Interval, Rope, RopeDelta, Transformer,
};
use lsp_types::{
    CodeActionOrCommand, CodeLens, Diagnostic, DocumentSymbolResponse, TextEdit,
};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use tracing::error;

use crate::editor::editor::{CommonAction, CursorInfo, Editor};
use crate::editor::gutter::FoldingRange;
use crate::editor::lines::{DocLines, DocLinesManager};
use crate::editor::screen_lines::ScreenLines;
use crate::editor::EditorViewKind;
use crate::{
    command::{CommandKind, InternalCommand, LapceCommand},
    config::LapceConfig,
    editor::{
        gutter::FoldingRanges,
        location::{EditorLocation, EditorPosition},
        EditorData,
    },
    find::{Find, FindProgress, FindResult},
    history::DocumentHistory,
    keypress::KeyPressFocus,
    main_split::Editors,
    panel::document_symbol::{
        DocumentSymbolViewData, SymbolData, SymbolInformationItemData,
    },
    window_tab::{CommonData, Focus, SignalManager},
    workspace::LapceWorkspace,
};

#[derive(Clone, Debug)]
pub struct DiagnosticData {
    pub expanded: RwSignal<bool>,
    pub diagnostics: RwSignal<im::Vector<Diagnostic>>,
    pub diagnostics_span: RwSignal<Spans<Diagnostic>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorDiagnostic {
    pub range: Option<(usize, usize)>,
    pub diagnostic: Diagnostic,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Debug)]
pub struct DocHistory {
    pub path: PathBuf,
    pub version: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Debug)]
pub enum DocContent {
    /// A file at some location. This can be a remote path.
    File { path: PathBuf, read_only: bool },
    /// A local document, which doens't need to be sync to the disk.
    Local,
    /// A document of an old version in the source control
    History(DocHistory),
    /// A new file which doesn't exist in the file system
    Scratch { id: BufferId, name: String },
}

impl DocContent {
    pub fn is_local(&self) -> bool {
        matches!(self, DocContent::Local)
    }

    pub fn is_file(&self) -> bool {
        matches!(self, DocContent::File { .. })
    }

    pub fn read_only(&self) -> bool {
        match self {
            DocContent::File { read_only, .. } => *read_only,
            DocContent::Local => false,
            DocContent::History(_) => true,
            DocContent::Scratch { .. } => false,
        }
    }

    pub fn path(&self) -> Option<&PathBuf> {
        match self {
            DocContent::File { path, .. } => Some(path),
            DocContent::Local => None,
            DocContent::History(_) => None,
            DocContent::Scratch { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocInfo {
    pub workspace: LapceWorkspace,
    pub path: PathBuf,
    pub scroll_offset: (f64, f64),
    pub cursor_offset: usize,
}

/// (Offset -> (Plugin the code actions are from, Code Actions))
pub type CodeActions =
    im::HashMap<usize, (PluginId, im::Vector<CodeActionOrCommand>)>;

pub type AllCodeLens = im::HashMap<usize, (PluginId, usize, im::Vector<CodeLens>)>;

#[derive(Clone)]
pub struct Doc {
    pub editor_id: EditorId,
    pub scope: Scope,
    pub buffer_id: BufferId,
    pub content: RwSignal<DocContent>,
    pub cache_rev: RwSignal<u64>,
    /// Whether the buffer's content has been loaded/initialized into the buffer.
    pub loaded: RwSignal<bool>,
    pub buffer: RwSignal<Buffer>,
    pub kind: RwSignal<EditorViewKind>,
    // pub syntax: RwSignal<Syntax>,
    // semantic_styles: RwSignal<Option<Spans<Style>>>,
    // semantic_previous_rs_id: RwSignal<Option<String>>,
    /// Inlay hints for the document
    // pub inlay_hints: RwSignal<Option<Spans<InlayHint>>>,
    /// Current completion lens text, if any.
    /// This will be displayed even on views that are not focused.
    // pub completion_lens: RwSignal<Option<String>>,
    /// (line, col)
    // pub completion_pos: RwSignal<(usize, usize)>,

    /// Current inline completion text, if any.
    /// This will be displayed even on views that are not focused.
    // pub inline_completion: RwSignal<Option<String>>,
    // /// (line, col)
    // pub inline_completion_pos: RwSignal<(usize, usize)>,

    /// (Offset -> (Plugin the code actions are from, Code Actions))
    pub code_actions: RwSignal<CodeActions>,

    pub code_lens: RwSignal<AllCodeLens>,

    // pub folding_ranges: RwSignal<FoldingRanges>,
    /// Stores information about different versions of the document from source control.
    histories: RwSignal<im::HashMap<String, DocumentHistory>>,
    pub head_changes: RwSignal<im::Vector<DiffLines>>,

    // line_styles: Rc<RefCell<LineStyles>>,
    // pub parser: Rc<RefCell<BracketParser>>,
    /// A cache for the sticky headers which maps a line to the lines it should show in the header.
    pub sticky_headers: Rc<RefCell<HashMap<usize, Option<Vec<usize>>>>>,

    // pub preedit: PreeditData,
    pub find_result: FindResult,

    /// The diagnostics for the document
    // pub diagnostics: DiagnosticData,
    editors: Editors,
    pub common: Rc<CommonData>,

    pub document_symbol_data: DocumentSymbolViewData,

    // pub lines: RwSignal<Lines>,
    // pub editor_style: RwSignal<EditorStyle>,
    // pub viewport: RwSignal<Rect>,
    pub lines: DocLinesManager,
    // pub screen_lines: RwSignal<ScreenLines>,
}
impl Doc {
    pub fn new(
        cx: Scope,
        path: PathBuf,
        diagnostics: DiagnosticData,
        editors: Editors,
        common: Rc<CommonData>,
    ) -> Self {
        let editor_id = EditorId::next();
        let syntax = Syntax::init(&path);
        let config = common.config.get_untracked();
        let rw_config = common.config;
        // let lines = cx.create_rw_signal(Lines::new(cx));
        let viewport = Rect::ZERO;
        let editor_style = EditorStyle::default();
        let buffer = cx.create_rw_signal(Buffer::new(""));
        let kind = cx.create_rw_signal(EditorViewKind::Normal);
        Doc {
            editor_id,
            kind,
            // viewport
            // editor_style,
            scope: cx,
            buffer_id: BufferId::next(),
            buffer,
            cache_rev: cx.create_rw_signal(0),
            content: cx.create_rw_signal(DocContent::File {
                path,
                read_only: false,
            }),
            loaded: cx.create_rw_signal(false),
            histories: cx.create_rw_signal(im::HashMap::new()),
            head_changes: cx.create_rw_signal(im::Vector::new()),
            sticky_headers: Rc::new(RefCell::new(HashMap::new())),
            code_actions: cx.create_rw_signal(im::HashMap::new()),
            find_result: FindResult::new(cx),
            // preedit: PreeditData::new(cx),
            editors,
            common,
            code_lens: cx.create_rw_signal(im::HashMap::new()),
            document_symbol_data: DocumentSymbolViewData::new(cx),
            // folding_ranges: cx.create_rw_signal(FoldingRanges::default()),
            // semantic_previous_rs_id: cx.create_rw_signal(None),
            lines: DocLinesManager::new(
                cx,
                diagnostics,
                syntax,
                BracketParser::new(
                    String::new(),
                    config.editor.bracket_pair_colorization,
                    config.editor.bracket_colorization_limit,
                ),
                viewport,
                editor_style,
                rw_config,
                buffer,
                kind,
            ),
        }
    }

    pub fn new_local(cx: Scope, editors: Editors, common: Rc<CommonData>) -> Doc {
        Self::new_content(cx, DocContent::Local, editors, common)
    }

    pub fn new_content(
        cx: Scope,
        content: DocContent,
        editors: Editors,
        common: Rc<CommonData>,
    ) -> Doc {
        let editor_id = EditorId::next();
        let cx = cx.create_child();
        let config = common.config.get_untracked();
        let rw_config = common.config;
        // let lines = cx.create_rw_signal(Lines::new(cx));
        let viewport = Rect::ZERO;
        let editor_style = EditorStyle::default();
        let diagnostics = DiagnosticData {
            expanded: cx.create_rw_signal(true),
            diagnostics: cx.create_rw_signal(im::Vector::new()),
            diagnostics_span: cx.create_rw_signal(SpansBuilder::new(0).build()),
        };
        let syntax = Syntax::plaintext();
        let buffer = cx.create_rw_signal(Buffer::new(""));
        let kind = cx.create_rw_signal(EditorViewKind::Normal);
        Self {
            editor_id,
            kind,
            scope: cx,
            buffer_id: BufferId::next(),
            buffer,
            cache_rev: cx.create_rw_signal(0),
            content: cx.create_rw_signal(content),
            histories: cx.create_rw_signal(im::HashMap::new()),
            head_changes: cx.create_rw_signal(im::Vector::new()),
            sticky_headers: Rc::new(RefCell::new(HashMap::new())),
            loaded: cx.create_rw_signal(true),
            find_result: FindResult::new(cx),
            code_actions: cx.create_rw_signal(im::HashMap::new()),
            // preedit: PreeditData::new(cx),
            editors,
            common,
            code_lens: cx.create_rw_signal(im::HashMap::new()),
            document_symbol_data: DocumentSymbolViewData::new(cx),
            // folding_ranges: cx.create_rw_signal(FoldingRanges::default()),
            // semantic_previous_rs_id: cx.create_rw_signal(None),
            // lines,
            // viewport,
            // editor_style,
            lines: DocLinesManager::new(
                cx,
                diagnostics,
                syntax,
                BracketParser::new(
                    String::new(),
                    config.editor.bracket_pair_colorization,
                    config.editor.bracket_colorization_limit,
                ),
                viewport,
                editor_style,
                rw_config,
                buffer,
                kind,
            ),
        }
    }

    pub fn new_history(
        cx: Scope,
        content: DocContent,
        editors: Editors,
        common: Rc<CommonData>,
    ) -> Doc {
        let editor_id = EditorId::next();
        let config = common.config.get_untracked();
        let rw_config = common.config;
        let syntax = if let DocContent::History(history) = &content {
            Syntax::init(&history.path)
        } else {
            Syntax::plaintext()
        };
        // let lines = cx.create_rw_signal(Lines::new(cx));
        let viewport = Rect::ZERO;
        let editor_style = EditorStyle::default();
        let diagnostics = DiagnosticData {
            expanded: cx.create_rw_signal(true),
            diagnostics: cx.create_rw_signal(im::Vector::new()),
            diagnostics_span: cx.create_rw_signal(SpansBuilder::new(0).build()),
        };
        let buffer = cx.create_rw_signal(Buffer::new(""));
        let kind = cx.create_rw_signal(EditorViewKind::Normal);
        Self {
            editor_id,
            kind,
            scope: cx,
            buffer_id: BufferId::next(),
            buffer,
            // syntax: cx.create_rw_signal(syntax),
            // line_styles: Rc::new(RefCell::new(HashMap::new())),
            // semantic_styles: cx.create_rw_signal(None),
            // inlay_hints: cx.create_rw_signal(None),
            // completion_lens: cx.create_rw_signal(None),
            // completion_pos: cx.create_rw_signal((0, 0)),
            // inline_completion: cx.create_rw_signal(None),
            // inline_completion_pos: cx.create_rw_signal((0, 0)),
            cache_rev: cx.create_rw_signal(0),
            content: cx.create_rw_signal(content),
            sticky_headers: Rc::new(RefCell::new(HashMap::new())),
            loaded: cx.create_rw_signal(true),
            histories: cx.create_rw_signal(im::HashMap::new()),
            head_changes: cx.create_rw_signal(im::Vector::new()),
            code_actions: cx.create_rw_signal(im::HashMap::new()),
            find_result: FindResult::new(cx),
            // preedit: PreeditData::new(cx),
            editors,
            common,
            code_lens: cx.create_rw_signal(im::HashMap::new()),
            document_symbol_data: DocumentSymbolViewData::new(cx),
            // folding_ranges: cx.create_rw_signal(FoldingRanges::default()),
            // semantic_previous_rs_id: cx.create_rw_signal(None),
            // lines,
            // viewport,
            // editor_style,
            lines: DocLinesManager::new(
                cx,
                diagnostics,
                syntax,
                BracketParser::new(
                    String::new(),
                    config.editor.bracket_pair_colorization,
                    config.editor.bracket_colorization_limit,
                ),
                viewport,
                editor_style,
                rw_config,
                buffer,
                kind,
            ),
        }
    }

    /// Create a styling instance for this doc
    pub fn styling(self: &Rc<Doc>) -> Rc<DocStyling> {
        Rc::new(DocStyling {
            config: self.common.config,
            doc: self.clone(),
        })
    }

    /// Create an [`Editor`] instance from this [`Doc`]. Note that this needs to be registered
    /// appropriately to create the [`EditorData`] and such.
    pub fn create_editor(self: &Rc<Doc>, cx: Scope, is_local: bool) -> Editor {
        let common = &self.common;
        let config = common.config.get_untracked();
        let modal = config.core.modal && !is_local;

        let register = common.register;
        // TODO: we could have these Rcs created once and stored somewhere, maybe on
        // common, to avoid recreating them everytime.
        let cursor_info = CursorInfo {
            blink_interval: Rc::new(move || config.editor.blink_interval()),
            blink_timer: common.window_common.cursor_blink_timer,
            hidden: common.window_common.hide_cursor,
            should_blink: Rc::new(should_blink(common.focus, common.keyboard_focus)),
        };
        let mut editor = Editor::new(cx, self.clone(), modal);

        editor.register = register;
        editor.cursor_info = cursor_info;
        editor.ime_allowed = common.window_common.ime_allowed;

        editor.recreate_view_effects();

        editor
    }

    fn editor_data(&self, id: EditorId) -> Option<EditorData> {
        self.editors.editor_untracked(id)
    }

    pub fn syntax(&self) -> Syntax {
        self.lines.with_untracked(|x| x.syntax.clone())
    }

    pub fn set_syntax(&self, syntax: Syntax) {
        batch(|| {
            self.lines.update(|x| {
                x.set_syntax(syntax);
            });
            // {
            //
            // }
            self.clear_text_cache();
            self.clear_sticky_headers_cache();
        });
    }

    /// Set the syntax highlighting this document should use.
    pub fn set_language(&self, language: LapceLanguage) {
        self.lines
            .update(|x| x.set_syntax(Syntax::from_language(language)));
    }

    pub fn find(&self) -> &Find {
        &self.common.find
    }

    /// Whether or not the underlying buffer is loaded
    pub fn loaded(&self) -> bool {
        self.loaded.get_untracked()
    }

    //// Initialize the content with some text, this marks the document as loaded.
    pub fn init_content(&self, content: Rope) {
        batch(|| {
            let syntax = self.syntax();
            self.buffer.update(|buffer| {
                buffer.init_content(content);
                buffer.detect_indent(|| {
                    IndentStyle::from_str(syntax.language.indent_unit())
                });
            });
            self.loaded.set(true);
            tracing::error!("init_content");
            self.on_update(None);
            self.retrieve_head();
        });
    }

    // fn init_parser(&self) {
    //
    //     // let code = self.buffer.get_untracked().to_string();
    //     // let syntax = self.syntax();
    //     // if syntax.styles.is_some() {
    //     //     self.parser.borrow_mut().update_code(
    //     //         code,
    //     //         &self.buffer.get_untracked(),
    //     //         Some(&syntax),
    //     //     );
    //     // } else {
    //     //     self.parser.borrow_mut().update_code(
    //     //         code,
    //     //         &self.buffer.get_untracked(),
    //     //         None,
    //     //     );
    //     // }
    // }

    /// Reload the document's content, and is what you should typically use when you want to *set*
    /// an existing document's content.
    pub fn reload(&self, content: Rope, set_pristine: bool) {
        // self.code_actions.clear();
        // self.inlay_hints = None;
        let delta = self
            .buffer
            .try_update(|buffer| buffer.reload(content, set_pristine))
            .unwrap();
        self.apply_deltas(&[delta]);
    }

    pub fn handle_file_changed(&self, content: Rope) {
        if self.is_pristine() {
            self.reload(content, true);
        }
    }

    pub fn do_insert(
        &self,
        cursor: &mut Cursor,
        s: &str,
        config: &LapceConfig,
    ) -> Vec<(Rope, RopeDelta, InvalLines)> {
        if self.content.with_untracked(|c| c.read_only()) {
            return Vec::new();
        }

        let old_cursor = cursor.mode.clone();
        let syntax = self.syntax();
        let deltas = self
            .buffer
            .try_update(|buffer| {
                Action::insert(
                    cursor,
                    buffer,
                    s,
                    &|buffer, c, offset| {
                        syntax_prev_unmatched(buffer, &syntax, c, offset)
                    },
                    config.editor.auto_closing_matching_pairs,
                    config.editor.auto_surround,
                )
            })
            .unwrap();
        // Keep track of the change in the cursor mode for undo/redo
        self.buffer.update(|buffer| {
            buffer.set_cursor_before(old_cursor);
            buffer.set_cursor_after(cursor.mode.clone());
        });
        self.apply_deltas(&deltas);
        deltas
    }

    pub fn do_raw_edit(
        &self,
        edits: &[(impl AsRef<Selection>, &str)],
        edit_type: EditType,
    ) -> Option<(Rope, RopeDelta, InvalLines)> {
        if self.content.with_untracked(|c| c.read_only()) {
            return None;
        }

        let (text, delta, inval_lines) = self
            .buffer
            .try_update(|buffer| buffer.edit(edits, edit_type))
            .unwrap();
        self.apply_deltas(&[(text.clone(), delta.clone(), inval_lines.clone())]);
        Some((text, delta, inval_lines))
    }

    pub fn do_edit(
        &self,
        cursor: &mut Cursor,
        cmd: &EditCommand,
        modal: bool,
        register: &mut Register,
        smart_tab: bool,
    ) -> Vec<(Rope, RopeDelta, InvalLines)> {
        if self.content.with_untracked(|c| c.read_only())
            && !cmd.not_changing_buffer()
        {
            return Vec::new();
        }

        let mut clipboard = SystemClipboard::new();
        let old_cursor = cursor.mode.clone();
        let syntax = self.syntax();
        let deltas = self
            .buffer
            .try_update(|buffer| {
                Action::do_edit(
                    cursor,
                    buffer,
                    cmd,
                    &mut clipboard,
                    register,
                    EditConf {
                        comment_token: syntax.language.comment_token(),
                        modal,
                        smart_tab,
                        keep_indent: true,
                        auto_indent: true,
                    },
                )
            })
            .unwrap();

        if !deltas.is_empty() {
            self.buffer.update(|buffer| {
                buffer.set_cursor_before(old_cursor);
                buffer.set_cursor_after(cursor.mode.clone());
            });
            self.apply_deltas(&deltas);
        }

        deltas
    }

    pub fn apply_deltas(&self, deltas: &[(Rope, RopeDelta, InvalLines)]) {
        let rev = self.rev() - deltas.len() as u64;
        batch(|| {
            for (i, (_, delta, inval)) in deltas.iter().enumerate() {
                self.update_styles(delta);
                self.update_inlay_hints(delta);
                self.update_diagnostics(delta);
                self.update_completion_lens(delta);
                self.update_find_result(delta);
                if let DocContent::File { path, .. } = self.content.get_untracked() {
                    self.update_breakpoints(delta, &path, &inval.old_text);
                    self.common.proxy.update(
                        path,
                        delta.clone(),
                        rev + i as u64 + 1,
                    );
                }
            }
        });

        // TODO(minor): We could avoid this potential allocation since most apply_delta callers are actually using a Vec
        // which we could reuse.
        // We use a smallvec because there is unlikely to be more than a couple of deltas
        let edits: SmallVec<[SyntaxEdit; 3]> = deltas
            .iter()
            .map(|(before_text, delta, _)| {
                SyntaxEdit::from_delta(before_text, delta.clone())
            })
            .collect();
        self.on_update(Some(edits));
    }

    pub fn is_pristine(&self) -> bool {
        self.buffer.with_untracked(|b| b.is_pristine())
    }

    /// Get the buffer's current revision. This is used to track whether the buffer has changed.
    pub fn rev(&self) -> u64 {
        self.buffer.with_untracked(|b| b.rev())
    }

    /// Get the buffer's line-ending.
    /// Note: this may not be the same as what the actual line endings in the file are, rather this
    /// is what the line-ending is set to (and what it will be saved as).
    pub fn line_ending(&self) -> LineEnding {
        self.buffer.with_untracked(|b| b.line_ending())
    }

    fn on_update(&self, edits: Option<SmallVec<[SyntaxEdit; 3]>>) {
        if self.content.get_untracked().is_local() {
            tracing::debug!("on_update cancle because doc is local");
            self.clear_text_cache();
            return;
        }
        batch(|| {
            self.trigger_syntax_change(edits);
            self.trigger_head_change();
            self.get_inlay_hints();
            self.find_result.reset();
            self.get_semantic_styles();
            // self.do_bracket_colorization();
            self.clear_code_actions();
            self.clear_style_cache();
            self.get_code_lens();
            self.get_document_symbol();
            self.get_folding_range();

            self.lines.update(|x| x.on_update_buffer());
        });
    }

    // fn do_bracket_colorization(&self) {
    //     self.lines.update(|x| x.do_bracket_colorization());
    // }

    pub fn do_text_edit(&self, edits: &[TextEdit]) {
        let edits = self.buffer.with_untracked(|buffer| {
            let edits = edits
                .iter()
                .map(|edit| {
                    let selection = lapce_core::selection::Selection::region(
                        buffer.offset_of_position(&edit.range.start),
                        buffer.offset_of_position(&edit.range.end),
                    );
                    (selection, edit.new_text.as_str())
                })
                .collect::<Vec<_>>();
            edits
        });
        self.do_raw_edit(&edits, EditType::Completion);
    }

    /// Update the styles after an edit, so the highlights are at the correct positions.
    /// This does not do a reparse of the document itself.
    fn update_styles(&self, delta: &RopeDelta) {
        self.lines.update(|x| x.update_styles(delta));
    }

    /// Update the inlay hints so their positions are correct after an edit.
    fn update_inlay_hints(&self, delta: &RopeDelta) {
        self.lines.update(|lines| {
            lines.update_inlay_hints(delta);
        });
    }

    pub fn trigger_syntax_change(&self, edits: Option<SmallVec<[SyntaxEdit; 3]>>) {
        let (rev, text) =
            self.buffer.with_untracked(|b| (b.rev(), b.text().clone()));

        let doc = self.clone();
        let send = create_ext_action(self.scope, move |syntax| {
            if doc.buffer.with_untracked(|b| b.rev()) == rev {
                doc.lines.update(|x| {
                    x.set_syntax(syntax);
                });
                // doc.do_bracket_colorization();
                doc.clear_sticky_headers_cache();
                doc.clear_text_cache();
            }
        });

        self.lines
            .update(|x| x.trigger_syntax_change(edits.clone()));
        let mut syntax = self.syntax();
        rayon::spawn(move || {
            syntax.parse(rev, text, edits.as_deref());
            send(syntax);
        });
    }

    fn clear_style_cache(&self) {
        self.clear_text_cache();
    }

    fn clear_code_actions(&self) {
        self.code_actions.update(|c| {
            c.clear();
        });
    }

    /// Inform any dependents on this document that they should clear any cached text.
    pub fn clear_text_cache(&self) {
        self.cache_rev.try_update(|cache_rev| {
            *cache_rev += 1;
            // TODO: ???
            // Update the text layouts within the callback so that those alerted to cache rev
            // will see the now empty layouts.
            // self.text_layouts.borrow_mut().clear(*cache_rev, None);
        });
    }

    fn clear_sticky_headers_cache(&self) {
        self.sticky_headers.borrow_mut().clear();
    }

    // /// Get the active style information, either the semantic styles or the
    // /// tree-sitter syntax styles.
    // fn styles(&self) -> Option<Spans<Style>> {
    //     self.lines.with_untracked(|lines| lines.styles())
    // }

    /// Get the style information for the particular line from semantic/syntax highlighting.
    /// This caches the result if possible.
    // pub fn line_style(&self, line: usize) -> Vec<LineStyle> {
    //     let buffer = self.buffer.get_untracked();
    //     self.lines
    //         .try_update(|x| x.line_style(line, &buffer))
    //         .unwrap()
    // }

    /// Request semantic styles for the buffer from the LSP through the proxy.
    // pub fn get_semantic_styles(&self) {
    pub fn get_semantic_full_styles(&self) {
        if !self.loaded() {
            return;
        }
        let path =
            if let DocContent::File { path, .. } = self.content.get_untracked() {
                path
            } else {
                return;
            };

        let (atomic_rev, rev, len) = self
            .buffer
            .with_untracked(|b| (b.atomic_rev(), b.rev(), b.len()));

        let doc = self.clone();
        let send = create_ext_action(self.scope, move |(styles, result_id)| {
            if let Some(styles) = styles {
                // error!("{:?}", styles);
                if doc.buffer.with_untracked(|b| b.rev()) == rev {
                    doc.lines
                        .update(|x| x.semantic_styles = Some((result_id, styles)));
                    doc.clear_style_cache();
                }
            }
        });
        self.common
            .proxy
            .get_semantic_tokens(path, move |(_, result)| {
                if let Ok(ProxyResponse::GetSemanticTokens { styles, result_id }) =
                    result
                {
                    if styles.styles.is_empty() {
                        send((None, result_id));
                        return;
                    }
                    if atomic_rev.load(atomic::Ordering::Acquire) != rev {
                        send((None, result_id));
                        return;
                    }
                    std::thread::spawn(move || {
                        let mut styles_span = SpansBuilder::new(len);
                        for style in styles.styles {
                            if atomic_rev.load(atomic::Ordering::Acquire) != rev {
                                send((None, result_id));
                                return;
                            }
                            styles_span.add_span(
                                Interval::new(style.start, style.end),
                                style.style,
                            );
                        }

                        let styles = styles_span.build();
                        send((Some(styles), result_id));
                    });
                } else {
                    send((None, None));
                }
            });
    }

    /// Request semantic styles for the buffer from the LSP through the proxy.
    pub fn get_semantic_styles(&self) {
        // todo
        self.get_semantic_full_styles();
        // let Some(id) = self.semantic_previous_rs_id.get_untracked() else {
        //     self.get_semantic_full_styles();
        //     return;
        // };
        // let path =
        //     if let DocContent::File { path, .. } = self.content.get_untracked() {
        //         path
        //     } else {
        //         return;
        //     };
        // let (atomic_rev, rev, len) = self
        //     .buffer
        //     .with_untracked(|b| (b.atomic_rev(), b.rev(), b.len()));
        //
        // let doc = self.clone();
        // let send = create_ext_action(self.scope, move |styles| {
        //     if let Some(styles) = styles {
        //         if doc.buffer.with_untracked(|b| b.rev()) == rev {
        //             doc.semantic_styles.set(Some(styles));
        //             doc.clear_style_cache();
        //         }
        //     }
        // });

        // self.common.proxy.get_semantic_tokens_delta(
        //     path,
        //     id,
        //     move |(_, _result)| {
        //         tracing::warn!("todo");
        // if let Ok(ProxyResponse::GetSemanticTokens { styles }) = result {
        //     if styles.styles.is_empty() {
        //         send(None);
        //         return;
        //     }
        //     if atomic_rev.load(atomic::Ordering::Acquire) != rev {
        //         send(None);
        //         return;
        //     }
        //     std::thread::spawn(move || {
        //         let mut styles_span = SpansBuilder::new(len);
        //         for style in styles.styles {
        //             if atomic_rev.load(atomic::Ordering::Acquire) != rev {
        //                 send(None);
        //                 return;
        //             }
        //             styles_span.add_span(
        //                 Interval::new(style.start, style.end),
        //                 style.style,
        //             );
        //         }

        //         let styles = styles_span.build();
        //         send(Some(styles));
        //     });
        // } else {
        //     send(None);
        // }
        // },
        // );
    }

    pub fn get_code_lens(&self) {
        let cx = self.scope;
        let doc = self.clone();
        self.code_lens.update(|code_lens| {
            code_lens.clear();
        });
        let rev = self.rev();
        if let DocContent::File { path, .. } = doc.content.get_untracked() {
            let send = create_ext_action(cx, move |result| {
                if rev != doc.rev() {
                    return;
                }
                if let Ok(ProxyResponse::GetCodeLensResponse { plugin_id, resp }) =
                    result
                {
                    let Some(codelens) = resp else {
                        return;
                    };
                    doc.code_lens.update(|code_lens| {
                        for codelens in codelens {
                            if codelens.command.is_none() {
                                continue;
                            }
                            let entry = code_lens
                                .entry(codelens.range.start.line as usize)
                                .or_insert_with(|| {
                                    (
                                        plugin_id,
                                        doc.buffer.with_untracked(|b| {
                                            b.offset_of_line(
                                                codelens.range.start.line as usize,
                                            )
                                        }),
                                        im::Vector::new(),
                                    )
                                });
                            entry.2.push_back(codelens);
                        }
                    });
                }
            });
            self.common.proxy.get_code_lens(path, move |(_, result)| {
                send(result);
            });
        }
    }

    pub fn get_document_symbol(&self) {
        let cx = self.scope;
        let doc = self.clone();
        let rev = self.rev();
        if let DocContent::File { path, .. } = doc.content.get_untracked() {
            let send = create_ext_action(cx, {
                let path = path.clone();
                move |result| {
                    if rev != doc.rev() {
                        return;
                    }
                    if let Ok(ProxyResponse::GetDocumentSymbols { resp }) = result {
                        let items: Vec<RwSignal<SymbolInformationItemData>> =
                            match resp {
                                DocumentSymbolResponse::Flat(_symbols) => {
                                    Vec::with_capacity(0)
                                }
                                DocumentSymbolResponse::Nested(symbols) => symbols
                                    .into_iter()
                                    .map(|x| {
                                        cx.create_rw_signal(
                                            SymbolInformationItemData::from((x, cx)),
                                        )
                                    })
                                    .collect(),
                            };
                        let symbol_new = Some(SymbolData::new(items, path, cx));
                        doc.document_symbol_data.virtual_list.update(|symbol| {
                            symbol.update(symbol_new);
                        });
                    }
                }
            });

            self.common
                .proxy
                .get_document_symbols(path, move |(_, result)| {
                    send(result);
                });
        }
    }

    /// Request inlay hints for the buffer from the LSP through the proxy.
    pub fn get_inlay_hints(&self) {
        if !self.loaded() {
            return;
        }

        let path =
            if let DocContent::File { path, .. } = self.content.get_untracked() {
                path
            } else {
                return;
            };

        let (buffer, rev, len) = self
            .buffer
            .with_untracked(|b| (b.clone(), b.rev(), b.len()));

        let doc = self.clone();
        let send = create_ext_action(self.scope, move |hints| {
            if doc.buffer.with_untracked(|b| b.rev()) == rev {
                doc.lines.update(|x| {
                    x.set_inlay_hints(hints);
                });
                doc.clear_text_cache();
            }
        });

        self.common.proxy.get_inlay_hints(path, move |(_, result)| {
            if let Ok(ProxyResponse::GetInlayHints { mut hints }) = result {
                // Sort the inlay hints by their position, as the LSP does not guarantee that it will
                // provide them in the order that they are in within the file
                // as well, Spans does not iterate in the order that they appear
                hints.sort_by(|left, right| left.position.cmp(&right.position));

                let mut hints_span = SpansBuilder::new(len);
                for hint in hints {
                    let offset = buffer.offset_of_position(&hint.position).min(len);
                    hints_span.add_span(
                        Interval::new(offset, (offset + 1).min(len)),
                        hint,
                    );
                }
                let hints = hints_span.build();
                send(hints);
            }
        });
    }

    pub fn diagnostics(&self) -> DiagnosticData {
        self.lines.with_untracked(|x| x.diagnostics.clone())
    }

    /// Update the diagnostics' positions after an edit so that they appear in the correct place.
    fn update_diagnostics(&self, delta: &RopeDelta) {
        self.lines.update(|x| x.update_diagnostics(delta));
    }

    /// init diagnostics offset ranges from lsp positions
    pub fn init_diagnostics(&self) {
        batch(|| {
            self.clear_text_cache();
            self.clear_code_actions();
            self.lines.update(|x| {
                x.init_diagnostics();
            });
        });
    }

    pub fn get_folding_range(&self) {
        let cx = self.scope;
        let doc = self.clone();
        let rev = self.rev();
        if let DocContent::File { path, .. } = doc.content.get_untracked() {
            let send = create_ext_action(cx, {
                move |result| {
                    if rev != doc.rev() {
                        return;
                    }
                    if let Ok(ProxyResponse::LspFoldingRangeResponse {
                        resp, ..
                    }) = result
                    {
                        let folding: Vec<FoldingRange> = resp
                            .unwrap_or_default()
                            .into_iter()
                            .map(FoldingRange::from_lsp)
                            .sorted_by(|x, y| x.start.line.cmp(&y.start.line))
                            .collect();
                        doc.lines.update(|symbol| {
                            symbol.update_folding_ranges(folding.into());
                        });
                        doc.clear_text_cache();
                    }
                }
            });

            self.common
                .proxy
                .get_lsp_folding_range(path, move |(_, result)| {
                    send(result);
                });
        }
    }

    /// Get the current completion lens text
    pub fn completion_lens(&self) -> Option<String> {
        self.lines.with_untracked(|x| x.completion_lens.clone())
    }

    pub fn set_completion_lens(
        &self,
        completion_lens: String,
        line: usize,
        col: usize,
    ) {
        self.lines
            .update(|x| x.set_completion_lens(completion_lens, line, col));
    }

    pub fn clear_completion_lens(&self) {
        self.lines.update(|x| x.clear_completion_lens());
    }

    fn update_breakpoints(&self, delta: &RopeDelta, path: &Path, old_text: &Rope) {
        if self
            .common
            .breakpoints
            .with_untracked(|breakpoints| breakpoints.contains_key(path))
        {
            self.common.breakpoints.update(|breakpoints| {
                if let Some(path_breakpoints) = breakpoints.get_mut(path) {
                    let mut transformer = Transformer::new(delta);
                    self.buffer.with_untracked(|buffer| {
                        *path_breakpoints = path_breakpoints
                            .clone()
                            .into_values()
                            .map(|mut b| {
                                let offset = old_text.offset_of_line(b.line);
                                let offset = transformer.transform(offset, false);
                                let line = buffer.line_of_offset(offset);
                                b.line = line;
                                b.offset = offset;
                                (b.line, b)
                            })
                            .collect();
                    });
                }
            });
        }
    }

    /// Update the completion lens position after an edit so that it appears in the correct place.
    pub fn update_completion_lens(&self, delta: &RopeDelta) {
        self.lines.update(|x| x.update_completion_lens(delta));
    }

    fn update_find_result(&self, delta: &RopeDelta) {
        self.find_result.occurrences.update(|s| {
            *s = s.apply_delta(delta, true, InsertDrift::Default);
        })
    }

    pub fn update_find(&self) {
        let find_rev = self.common.find.rev.get_untracked();
        if self.find_result.find_rev.get_untracked() != find_rev {
            if self
                .common
                .find
                .search_string
                .with_untracked(|search_string| {
                    search_string
                        .as_ref()
                        .map(|s| s.content.is_empty())
                        .unwrap_or(true)
                })
            {
                self.find_result.occurrences.set(Selection::new());
            }
            self.find_result.reset();
            self.find_result.find_rev.set(find_rev);
        }

        if self.find_result.progress.get_untracked() != FindProgress::Started {
            return;
        }

        let search = self.common.find.search_string.get_untracked();
        let search = match search {
            Some(search) => search,
            None => return,
        };
        if search.content.is_empty() {
            return;
        }

        self.find_result
            .progress
            .set(FindProgress::InProgress(Selection::new()));

        let find_result = self.find_result.clone();
        let find_rev_signal = self.common.find.rev;
        let triggered_by_changes = self.common.find.triggered_by_changes;

        let path = self.content.get_untracked().path().cloned();
        let common = self.common.clone();
        let send = create_ext_action(self.scope, move |occurrences: Selection| {
            if let (false, Some(path), true, true) = (
                occurrences.regions().is_empty(),
                &path,
                find_rev_signal.get_untracked() == find_rev,
                triggered_by_changes.get_untracked(),
            ) {
                triggered_by_changes.set(false);
                common.internal_command.send(InternalCommand::GoToLocation {
                    location: EditorLocation {
                        path: path.clone(),
                        position: Some(EditorPosition::Offset(
                            occurrences.regions()[0].start,
                        )),
                        scroll_offset: None,
                        ignore_unconfirmed: false,
                        same_editor_tab: false,
                    },
                });
            }
            find_result.occurrences.set(occurrences);
            find_result.progress.set(FindProgress::Ready);
        });

        let text = self.buffer.with_untracked(|b| b.text().clone());
        let case_matching = self.common.find.case_matching.get_untracked();
        let whole_words = self.common.find.whole_words.get_untracked();
        rayon::spawn(move || {
            let mut occurrences = Selection::new();
            Find::find(
                &text,
                &search,
                0,
                text.len(),
                case_matching,
                whole_words,
                true,
                &mut occurrences,
            );
            send(occurrences);
        });
    }

    /// Get the sticky headers for a particular line, creating them if necessary.
    pub fn sticky_headers(&self, line: usize) -> Option<Vec<usize>> {
        if let Some(lines) = self.sticky_headers.borrow().get(&line) {
            return lines.clone();
        }
        let lines = self.buffer.with_untracked(|buffer| {
            let offset = buffer.offset_of_line(line + 1);
            self.syntax().sticky_headers(offset).map(|offsets| {
                offsets
                    .iter()
                    .filter_map(|offset| {
                        let l = buffer.line_of_offset(*offset);
                        if l <= line {
                            Some(l)
                        } else {
                            None
                        }
                    })
                    .dedup()
                    .sorted()
                    .collect()
            })
        });
        self.sticky_headers.borrow_mut().insert(line, lines.clone());
        lines
    }

    pub fn head_changes(&self) -> RwSignal<im::Vector<DiffLines>> {
        self.head_changes
    }

    /// Retrieve the `head` version of the buffer
    pub fn retrieve_head(&self) {
        if let DocContent::File { path, .. } = self.content.get_untracked() {
            let histories = self.histories;

            let send = {
                let path = path.clone();
                let doc = self.clone();
                create_ext_action(self.scope, move |result| {
                    if let Ok(ProxyResponse::BufferHeadResponse {
                        content, ..
                    }) = result
                    {
                        let hisotry = DocumentHistory::new(
                            path.clone(),
                            "head".to_string(),
                            &content,
                        );
                        histories.update(|histories| {
                            histories.insert("head".to_string(), hisotry);
                        });

                        doc.trigger_head_change();
                    }
                })
            };

            let path = path.clone();
            let proxy = self.common.proxy.clone();
            std::thread::spawn(move || {
                proxy.get_buffer_head(path, move |(_, result)| {
                    send(result);
                });
            });
        }
    }

    pub fn trigger_head_change(&self) {
        let history = if let Some(text) =
            self.histories.with_untracked(|histories| {
                histories
                    .get("head")
                    .map(|history| history.buffer.text().clone())
            }) {
            text
        } else {
            return;
        };

        let rev = self.rev();
        let left_rope = history;
        let (atomic_rev, right_rope) = self
            .buffer
            .with_untracked(|b| (b.atomic_rev(), b.text().clone()));

        let send = {
            let atomic_rev = atomic_rev.clone();
            let head_changes = self.head_changes;
            create_ext_action(self.scope, move |changes| {
                let changes = if let Some(changes) = changes {
                    changes
                } else {
                    return;
                };

                if atomic_rev.load(atomic::Ordering::Acquire) != rev {
                    return;
                }

                head_changes.set(changes);
            })
        };

        rayon::spawn(move || {
            let changes =
                rope_diff(left_rope, right_rope, rev, atomic_rev.clone(), None);
            send(changes.map(im::Vector::from));
        });
    }

    pub fn save(&self, after_action: impl FnOnce() + 'static) {
        let content = self.content.get_untracked();
        if let DocContent::File { path, .. } = content {
            let rev = self.rev();
            let buffer = self.buffer;
            let send = create_ext_action(self.scope, move |result| {
                if let Ok(ProxyResponse::SaveResponse {}) = result {
                    let current_rev = buffer.with_untracked(|buffer| buffer.rev());
                    if current_rev == rev {
                        buffer.update(|buffer| {
                            buffer.set_pristine();
                        });
                        after_action();
                    }
                }
            });

            self.common.proxy.save(rev, path, true, move |(_, result)| {
                send(result);
            })
        }
    }

    pub fn set_inline_completion(
        &self,
        inline_completion: String,
        line: usize,
        col: usize,
    ) {
        // TODO: more granular invalidation
        batch(|| {
            self.lines
                .update(|x| x.set_inline_completion(inline_completion, line, col));
            self.clear_text_cache();
        });
    }

    pub fn clear_inline_completion(&self) {
        batch(|| {
            self.lines.update(|x| x.clear_inline_completion());
            self.clear_text_cache();
        });
    }

    pub fn update_inline_completion(&self, delta: &RopeDelta) {
        self.lines.update(|x| {
            x.update_inline_completion(delta);
        })
    }

    pub fn code_actions(&self) -> RwSignal<CodeActions> {
        self.code_actions
    }

    /// Returns the offsets of the brackets enclosing the given offset.
    /// Uses a language aware algorithm if syntax support is available for the current language,
    /// else falls back to a language unaware algorithm.
    pub fn find_enclosing_brackets(&self, offset: usize) -> Option<(usize, usize)> {
        let rev = self.rev();
        let syntax = self.syntax();
        (!syntax.text.is_empty() && syntax.rev == rev)
            .then(|| syntax.find_enclosing_pair(offset))
            // If syntax.text is empty, either the buffer is empty or we don't have syntax support
            // for the current language.
            // Try a language unaware search for enclosing brackets in case it is the latter.
            .unwrap_or_else(|| {
                self.buffer.with_untracked(|buffer| {
                    WordCursor::new(buffer.text(), offset).find_enclosing_pair()
                })
            })
    }
}
impl Doc {
    pub fn text(&self) -> Rope {
        self.buffer.with_untracked(|buffer| buffer.text().clone())
    }

    // pub fn lines(&self) -> DocLinesManager {
    //     self.lines
    // }
    pub fn rope_text(&self) -> RopeTextVal {
        RopeTextVal::new(self.text())
    }

    pub fn cache_rev(&self) -> RwSignal<u64> {
        self.cache_rev
    }

    // fn visual_line_of_line(&self, line: usize) -> usize {
    //     self.folding_ranges
    //         .with_untracked(|x| x.get_folded_range().visual_line(line))
    // }

    pub fn find_unmatched(&self, offset: usize, previous: bool, ch: char) -> usize {
        let syntax = self.syntax();
        if syntax.layers.is_some() {
            syntax
                .find_tag(offset, previous, &CharBuffer::from(ch))
                .unwrap_or(offset)
        } else {
            let text = self.text();
            let mut cursor = WordCursor::new(&text, offset);
            let new_offset = if previous {
                cursor.previous_unmatched(ch)
            } else {
                cursor.next_unmatched(ch)
            };

            new_offset.unwrap_or(offset)
        }
    }

    pub fn find_matching_pair(&self, offset: usize) -> usize {
        let syntax = self.syntax();
        if syntax.layers.is_some() {
            syntax.find_matching_pair(offset).unwrap_or(offset)
        } else {
            let text = self.text();
            WordCursor::new(&text, offset)
                .match_pairs()
                .unwrap_or(offset)
        }
    }

    pub fn preedit(&self) -> PreeditData {
        self.lines.with_untracked(|x| x.preedit.clone())
    }

    pub fn compute_screen_lines(
        &self,
        _editor: &Editor,
        _base: RwSignal<ScreenLinesBase>,
    ) -> ScreenLines {
        todo!()
        // let Some(editor_data) = self.editor_data(editor.id()) else {
        //     return ScreenLines {
        //         lines: Default::default(),
        //         info: Default::default(),
        //         diff_sections: Default::default(),
        //         base,
        //     };
        // };
        //
        // // compute_screen_lines(
        // //     self.common.config,
        // //     base,
        // //     editor_data.kind(),
        // //     &editor_data.doc_signal().get(),
        // //     editor.text_prov(),
        // //     editor.config_id(),
        // // )
        // let lines = editor_data
        //     .doc_signal()
        //     .with_untracked(|x| x.lines.get_untracked());
        // compute_screen_lines(lines)
    }

    pub fn run_command(
        &self,
        ed: &Editor,
        cmd: &Command,
        count: Option<usize>,
        modifiers: Modifiers,
    ) -> CommandExecuted {
        let Some(editor_data) = self.editor_data(ed.id()) else {
            return CommandExecuted::No;
        };

        let cmd = CommandKind::from(cmd.clone());
        let cmd = LapceCommand {
            kind: cmd,
            data: None,
        };
        editor_data.run_command(&cmd, count, modifiers)
    }

    pub fn receive_char(&self, ed: &Editor, c: &str) {
        let Some(editor_data) = self.editor_data(ed.id()) else {
            return;
        };

        editor_data.receive_char(c);
    }

    pub fn edit(
        &self,
        iter: &mut dyn Iterator<Item = (Selection, &str)>,
        edit_type: EditType,
    ) {
        let delta = self
            .buffer
            .try_update(|buffer| buffer.edit(iter, edit_type))
            .unwrap();
        self.apply_deltas(&[delta]);
    }

    pub fn editor_id(&self) -> EditorId {
        self.editor_id
    }

    // pub fn viewport(&self) -> RwSignal<Rect> {
    //     self.viewport
    // }

    // pub fn editor_style(&self) -> RwSignal<EditorStyle> {
    //     self.editor_style
    // }
}

// impl DocumentPhantom for Doc {
//     fn phantom_text(&self, _: &EditorStyle, _line: usize) -> PhantomTextLine {
//         // let config = &self.common.config.get_untracked();
//         //
//         // let buffer = self.buffer.get_untracked();
//         // let (start_offset, end_offset) =
//         //     (buffer.offset_of_line(line), buffer.offset_of_line(line + 1));
//         //
//         // let origin_text_len = end_offset - start_offset;
//         // // lsp返回的字符包括换行符，现在长度不考虑，后续会有问题
//         // // let line_ending = buffer.line_ending().get_chars().len();
//         // // if origin_text_len >= line_ending {
//         // //     origin_text_len -= line_ending;
//         // // }
//         // // if line == 8 {
//         // //     tracing::info!("start_offset={start_offset} end_offset={end_offset} line_ending={line_ending} origin_text_len={origin_text_len}");
//         // // }
//         //
//         // let folded_ranges = self
//         //     .folding_ranges
//         //     .get_untracked()
//         //     .get_folded_range_by_line(line as u32);
//         //
//         // let inlay_hints = self.linesinlay_hints.get_untracked();
//         // // If hints are enabled, and the hints field is filled, then get the hints for this line
//         // // and convert them into PhantomText instances
//         // let hints = config
//         //     .editor
//         //     .enable_inlay_hints
//         //     .then_some(())
//         //     .and(inlay_hints.as_ref())
//         //     .map(|hints| hints.iter_chunks(start_offset..end_offset))
//         //     .into_iter()
//         //     .flatten()
//         //     .filter(|(interval, hint)| {
//         //         interval.start >= start_offset
//         //             && interval.start < end_offset
//         //             && !folded_ranges.contain_position(hint.position)
//         //     })
//         //     .map(|(interval, inlay_hint)| {
//         //         let (col, affinity) = {
//         //             let mut cursor =
//         //                 lapce_xi_rope::Cursor::new(buffer.text(), interval.start);
//         //
//         //             let next_char = cursor.peek_next_codepoint();
//         //             let prev_char = cursor.prev_codepoint();
//         //
//         //             let mut affinity = None;
//         //             if let Some(prev_char) = prev_char {
//         //                 let c = get_char_property(prev_char);
//         //                 if c == CharClassification::Other {
//         //                     affinity = Some(CursorAffinity::Backward)
//         //                 } else if matches!(
//         //                     c,
//         //                     CharClassification::Lf
//         //                         | CharClassification::Cr
//         //                         | CharClassification::Space
//         //                 ) {
//         //                     affinity = Some(CursorAffinity::Forward)
//         //                 }
//         //             };
//         //             if affinity.is_none() {
//         //                 if let Some(next_char) = next_char {
//         //                     let c = get_char_property(next_char);
//         //                     if c == CharClassification::Other {
//         //                         affinity = Some(CursorAffinity::Forward)
//         //                     } else if matches!(
//         //                         c,
//         //                         CharClassification::Lf
//         //                             | CharClassification::Cr
//         //                             | CharClassification::Space
//         //                     ) {
//         //                         affinity = Some(CursorAffinity::Backward)
//         //                     }
//         //                 }
//         //             }
//         //
//         //             let (_, col) = buffer.offset_to_line_col(interval.start);
//         //             (col, affinity)
//         //         };
//         //         let mut text = match &inlay_hint.label {
//         //             InlayHintLabel::String(label) => label.to_string(),
//         //             InlayHintLabel::LabelParts(parts) => {
//         //                 parts.iter().map(|p| &p.value).join("")
//         //             }
//         //         };
//         //         match (text.starts_with(':'), text.ends_with(':')) {
//         //             (true, true) => {
//         //                 text.push(' ');
//         //             }
//         //             (true, false) => {
//         //                 text.push(' ');
//         //             }
//         //             (false, true) => {
//         //                 text = format!(" {} ", text);
//         //             }
//         //             (false, false) => {
//         //                 text = format!(" {}", text);
//         //             }
//         //         }
//         //         PhantomText {
//         //             kind: PhantomTextKind::InlayHint,
//         //             col,
//         //             text,
//         //             affinity,
//         //             fg: Some(config.color(LapceColor::INLAY_HINT_FOREGROUND)),
//         //             // font_family: Some(config.editor.inlay_hint_font_family()),
//         //             font_size: Some(config.editor.inlay_hint_font_size()),
//         //             bg: Some(config.color(LapceColor::INLAY_HINT_BACKGROUND)),
//         //             under_line: None,
//         //             final_col: col,
//         //             line,
//         //             merge_col: col,
//         //         }
//         //     });
//         // // You're quite unlikely to have more than six hints on a single line
//         // // this later has the diagnostics added onto it, but that's still likely to be below six
//         // // overall.
//         // let mut text: SmallVec<[PhantomText; 6]> = hints.collect();
//         //
//         // // If error lens is enabled, and the diagnostics field is filled, then get the diagnostics
//         // // that end on this line which have a severity worse than HINT and convert them into
//         // // PhantomText instances
//         //
//         // let mut diag_text: SmallVec<[PhantomText; 6]> = config
//         //     .editor
//         //     .enable_error_lens
//         //     .then_some(())
//         //     .map(|_| self.diagnostics.diagnostics_span.get_untracked())
//         //     .map(|diags| {
//         //         diags
//         //             .iter_chunks(start_offset..end_offset)
//         //             .filter_map(|(iv, diag)| {
//         //                 let end = iv.end();
//         //                 let end_line = buffer.line_of_offset(end);
//         //                 if end_line == line
//         //                     && diag.severity < Some(DiagnosticSeverity::HINT)
//         //                     && !folded_ranges.contain_position(diag.range.start)
//         //                     && !folded_ranges.contain_position(diag.range.end)
//         //                 {
//         //                     let fg = {
//         //                         let severity = diag
//         //                             .severity
//         //                             .unwrap_or(DiagnosticSeverity::WARNING);
//         //                         let theme_prop = if severity
//         //                             == DiagnosticSeverity::ERROR
//         //                         {
//         //                             LapceColor::ERROR_LENS_ERROR_FOREGROUND
//         //                         } else if severity == DiagnosticSeverity::WARNING {
//         //                             LapceColor::ERROR_LENS_WARNING_FOREGROUND
//         //                         } else {
//         //                             // information + hint (if we keep that) + things without a severity
//         //                             LapceColor::ERROR_LENS_OTHER_FOREGROUND
//         //                         };
//         //
//         //                         config.color(theme_prop)
//         //                     };
//         //
//         //                     let text = if config.editor.only_render_error_styling {
//         //                         "".to_string()
//         //                     } else if config.editor.error_lens_multiline {
//         //                         format!("    {}", diag.message)
//         //                     } else {
//         //                         format!("    {}", diag.message.lines().join(" "))
//         //                     };
//         //                     Some(PhantomText {
//         //                         kind: PhantomTextKind::Diagnostic,
//         //                         col: end_offset - start_offset,
//         //                         affinity: Some(CursorAffinity::Backward),
//         //                         text,
//         //                         fg: Some(fg),
//         //                         font_size: Some(
//         //                             config.editor.error_lens_font_size(),
//         //                         ),
//         //                         bg: None,
//         //                         under_line: None,
//         //                         final_col: end_offset - start_offset,
//         //                         line,
//         //                         merge_col: end_offset - start_offset,
//         //                     })
//         //                 } else {
//         //                     None
//         //                 }
//         //             })
//         //             .collect::<SmallVec<[PhantomText; 6]>>()
//         //     })
//         //     .unwrap_or_default();
//         //
//         // text.append(&mut diag_text);
//         //
//         // let (completion_line, completion_col) = self.completion_pos.get_untracked();
//         // let completion_text = config
//         //     .editor
//         //     .enable_completion_lens
//         //     .then_some(())
//         //     .and(self.completion_lens.get_untracked())
//         //     // TODO: We're probably missing on various useful completion things to include here!
//         //     .filter(|_| {
//         //         line == completion_line
//         //             && !folded_ranges.contain_position(Position {
//         //                 line: completion_line as u32,
//         //                 character: completion_col as u32,
//         //             })
//         //     })
//         //     .map(|completion| PhantomText {
//         //         kind: PhantomTextKind::Completion,
//         //         col: completion_col,
//         //         text: completion.clone(),
//         //         fg: Some(config.color(LapceColor::COMPLETION_LENS_FOREGROUND)),
//         //         font_size: Some(config.editor.completion_lens_font_size()),
//         //         affinity: Some(CursorAffinity::Backward),
//         //         // font_family: Some(config.editor.completion_lens_font_family()),
//         //         bg: None,
//         //         under_line: None,
//         //         final_col: completion_col,
//         //         line,
//         //         merge_col: completion_col,
//         //         // TODO: italics?
//         //     });
//         // if let Some(completion_text) = completion_text {
//         //     text.push(completion_text);
//         // }
//         //
//         // // TODO: don't display completion lens and inline completion at the same time
//         // // and/or merge them so that they can be shifted between like multiple inline completions
//         // // can
//         // let (inline_completion_line, inline_completion_col) =
//         //     self.inline_completion_pos.get_untracked();
//         // let inline_completion_text = config
//         //     .editor
//         //     .enable_inline_completion
//         //     .then_some(())
//         //     .and(self.inline_completion.get_untracked())
//         //     .filter(|_| {
//         //         line == inline_completion_line
//         //             && !folded_ranges.contain_position(Position {
//         //                 line: inline_completion_line as u32,
//         //                 character: inline_completion_col as u32,
//         //             })
//         //     })
//         //     .map(|completion| PhantomText {
//         //         kind: PhantomTextKind::Completion,
//         //         col: inline_completion_col,
//         //         text: completion.clone(),
//         //         affinity: Some(CursorAffinity::Backward),
//         //         fg: Some(config.color(LapceColor::COMPLETION_LENS_FOREGROUND)),
//         //         font_size: Some(config.editor.completion_lens_font_size()),
//         //         // font_family: Some(config.editor.completion_lens_font_family()),
//         //         bg: None,
//         //         under_line: None,
//         //         final_col: inline_completion_col,
//         //         line,
//         //         merge_col: inline_completion_col,
//         //         // TODO: italics?
//         //     });
//         // if let Some(inline_completion_text) = inline_completion_text {
//         //     text.push(inline_completion_text);
//         // }
//         //
//         // // todo filter by folded?
//         // if let Some(preedit) = self
//         //     .preedit_phantom(Some(config.color(LapceColor::EDITOR_FOREGROUND)), line)
//         // {
//         //     text.push(preedit)
//         // }
//         // text.extend(folded_ranges.into_phantom_text(&buffer, config, line));
//         //
//         // PhantomTextLine::new(line, origin_text_len, text)
//         todo!()
//     }
//
//     // fn has_multiline_phantom(&self, _: EditorId, _: &EditorStyle) -> bool {
//     //     // TODO: actually check
//     //     true
//     // }
// }
impl CommonAction for Doc {
    fn exec_motion_mode(
        &self,
        _ed: &Editor,
        cursor: &mut Cursor,
        motion_mode: MotionMode,
        range: Range<usize>,
        is_vertical: bool,
        register: &mut Register,
    ) {
        let deltas = self
            .buffer
            .try_update(move |buffer| {
                Action::execute_motion_mode(
                    cursor,
                    buffer,
                    motion_mode,
                    range,
                    is_vertical,
                    register,
                )
            })
            .unwrap();
        self.apply_deltas(&deltas);
    }

    fn do_edit(
        &self,
        _ed: &Editor,
        cursor: &mut Cursor,
        cmd: &EditCommand,
        modal: bool,
        register: &mut Register,
        smart_tab: bool,
    ) -> bool {
        let deltas = Doc::do_edit(self, cursor, cmd, modal, register, smart_tab);
        !deltas.is_empty()
    }
}

#[derive(Clone)]
pub struct DocStyling {
    config: ReadSignal<Arc<LapceConfig>>,
    doc: Rc<Doc>,
}
impl DocStyling {
    // fn apply_colorization(
    //     &self,
    //     line: usize,
    //     attrs: &Attrs,
    //     attrs_list: &mut AttrsList,
    //     phantom_text: &PhantomTextLine,
    // ) {
    //     let config = self.config.get_untracked();
    //     // todo it always empty??
    //     if let Some(bracket_styles) = self.doc.parser.borrow().bracket_pos.get(&line)
    //     {
    //         tracing::error!("bracket_styles.len={}", bracket_styles.len());
    //         for bracket_style in bracket_styles.iter() {
    //             // tracing::info!("{line} {:?}", bracket_style);
    //             if let Some(fg_color) = bracket_style.style.fg_color.as_ref() {
    //                 if let Some(fg_color) = config.style_color(fg_color) {
    //                     let (Some(start), Some(end)) = (
    //                         phantom_text.col_at(bracket_style.start),
    //                         phantom_text.col_at(bracket_style.end),
    //                     ) else {
    //                         continue;
    //                     };
    //                     tracing::info!("{line} {:?} {start} {end}", bracket_style);
    //                     attrs_list.add_span(start..end, attrs.color(fg_color));
    //                 }
    //             }
    //         }
    //     }
    // }
}

impl Styling for Doc {
    fn id(&self) -> u64 {
        self.common.config.with_untracked(|config| config.id)
    }

    fn font_size(&self, _line: usize) -> usize {
        self.common
            .config
            .with_untracked(|config| config.editor.font_size())
    }

    fn line_height(&self, _line: usize) -> f32 {
        self.common
            .config
            .with_untracked(|config| config.editor.line_height()) as f32
    }

    fn font_family(
        &self,
        _line: usize,
    ) -> std::borrow::Cow<[floem::text::FamilyOwned]> {
        // TODO: cache this
        Cow::Owned(self.common.config.with_untracked(|config| {
            FamilyOwned::parse_list(&config.editor.font_family).collect()
        }))
    }

    fn weight(&self, _: EditorId, _line: usize) -> floem::text::Weight {
        floem::text::Weight::NORMAL
    }

    fn italic_style(&self, _: EditorId, _line: usize) -> floem::text::Style {
        floem::text::Style::Normal
    }

    fn stretch(&self, _: EditorId, _line: usize) -> floem::text::Stretch {
        floem::text::Stretch::Normal
    }

    fn indent_line(&self, line: usize, line_content: &str) -> usize {
        if line_content.trim().is_empty() {
            let text = self.rope_text();
            let offset = text.offset_of_line(line);
            if let Some(offset) = self.syntax().parent_offset(offset) {
                return text.line_of_offset(offset);
            }
        }

        line
    }

    fn tab_width(&self, _: EditorId, _line: usize) -> usize {
        self.common
            .config
            .with_untracked(|config| config.editor.tab_width)
    }

    fn atomic_soft_tabs(&self, _: EditorId, _line: usize) -> bool {
        self.common
            .config
            .with_untracked(|config| config.editor.atomic_soft_tabs)
    }

    fn line_styles(&self, _line: usize) -> Vec<(usize, usize, Color)> {
        // let config = self.common.config.get_untracked();
        // let mut styles: Vec<(usize, usize, Color)> = self
        //     .line_style(line)
        //     .iter()
        //     .filter_map(|line_style| {
        //         if let Some(fg_color) = line_style.style.fg_color.as_ref() {
        //             if let Some(fg_color) = config.style_color(fg_color) {
        //                 return Some((line_style.start, line_style.end, fg_color));
        //             }
        //         }
        //         None
        //     })
        //     .collect();
        //
        // if let Some(bracket_styles) =
        //     self.lines.with_untracked(|x| x.line_styles(line))
        // {
        //     tracing::error!("bracket_styles.len={}", bracket_styles.len());
        //     styles.append(
        //         &mut bracket_styles
        //             .iter()
        //             .filter_map(|bracket_style| {
        //                 if let Some(fg_color) = bracket_style.style.fg_color.as_ref()
        //                 {
        //                     if let Some(fg_color) = config.style_color(fg_color) {
        //                         return Some((
        //                             bracket_style.start,
        //                             bracket_style.end,
        //                             fg_color,
        //                         ));
        //                     }
        //                 }
        //                 None
        //             })
        //             .collect(),
        //     );
        // }
        // styles
        vec![]
    }

    // fn apply_attr_styles(
    //     &self,
    //     line: usize,
    //     default: Attrs,
    //     attrs_list: &mut AttrsList,
    //     phantom_text: &PhantomTextLine,
    //     collapsed_line_col: usize,
    // ) {
    //     let config = self.doc.common.config.get_untracked();
    //
    //     // todo
    //     self.apply_colorization(line, &default, attrs_list, phantom_text);
    //
    //     // calculate style of origin text, for example: `self`
    //     for line_style in self.doc.line_style(line).iter() {
    //         if let Some(fg_color) = line_style.style.fg_color.as_ref() {
    //             if let Some(fg_color) = config.style_color(fg_color) {
    //                 let (Some(start), Some(end)) = (
    //                     phantom_text.col_at(line_style.start),
    //                     phantom_text.col_at(line_style.end),
    //                 ) else {
    //                     continue;
    //                 };
    //                 attrs_list.add_span(
    //                     start + collapsed_line_col..end + collapsed_line_col,
    //                     default.color(fg_color),
    //                 );
    //             }
    //         }
    //     }
    // }

    fn paint_caret(&self, edid: EditorId, _line: usize) -> bool {
        let Some(e_data) = self.editor_data(edid) else {
            return true;
        };

        // If the find is active, then we don't want to paint the caret
        !e_data.find_focus.get_untracked()
    }
}

impl Styling for DocStyling {
    fn id(&self) -> u64 {
        self.config.with_untracked(|config| config.id)
    }

    fn font_size(&self, _line: usize) -> usize {
        self.config
            .with_untracked(|config| config.editor.font_size())
    }

    fn line_height(&self, _line: usize) -> f32 {
        self.config
            .with_untracked(|config| config.editor.line_height()) as f32
    }

    fn font_family(
        &self,
        _line: usize,
    ) -> std::borrow::Cow<[floem::text::FamilyOwned]> {
        // TODO: cache this
        Cow::Owned(self.config.with_untracked(|config| {
            FamilyOwned::parse_list(&config.editor.font_family).collect()
        }))
    }

    fn weight(&self, _: EditorId, _line: usize) -> floem::text::Weight {
        floem::text::Weight::NORMAL
    }

    fn italic_style(&self, _: EditorId, _line: usize) -> floem::text::Style {
        floem::text::Style::Normal
    }

    fn stretch(&self, _: EditorId, _line: usize) -> floem::text::Stretch {
        floem::text::Stretch::Normal
    }

    fn indent_line(&self, line: usize, line_content: &str) -> usize {
        if line_content.trim().is_empty() {
            let text = self.doc.rope_text();
            let offset = text.offset_of_line(line);
            if let Some(offset) = self.doc.syntax().parent_offset(offset) {
                return text.line_of_offset(offset);
            }
        }

        line
    }

    fn tab_width(&self, _: EditorId, _line: usize) -> usize {
        self.config.with_untracked(|config| config.editor.tab_width)
    }

    fn atomic_soft_tabs(&self, _: EditorId, _line: usize) -> bool {
        self.config
            .with_untracked(|config| config.editor.atomic_soft_tabs)
    }

    fn line_styles(&self, line: usize) -> Vec<(usize, usize, Color)> {
        self.doc.line_styles(line)
    }

    fn paint_caret(&self, edid: EditorId, _line: usize) -> bool {
        let Some(e_data) = self.doc.editor_data(edid) else {
            return true;
        };

        // If the find is active, then we don't want to paint the caret
        !e_data.find_focus.get_untracked()
    }
}

impl std::fmt::Debug for Doc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("Document {:?}", self.buffer_id))
    }
}

/// Get the previous unmatched character `c` from the `offset` using `syntax` if applicable
fn syntax_prev_unmatched(
    buffer: &Buffer,
    syntax: &Syntax,
    c: char,
    offset: usize,
) -> Option<usize> {
    if syntax.layers.is_some() {
        syntax.find_tag(offset, true, &CharBuffer::new(c))
    } else {
        WordCursor::new(buffer.text(), offset).previous_unmatched(c)
    }
}

fn should_blink(
    _focus: SignalManager<Focus>,
    _keyboard_focus: RwSignal<Option<ViewId>>,
) -> impl Fn() -> bool {
    move || {
        // let Some(focus) = _focus.try_get_untracked() else {
        //     return false;
        // };
        // if matches!(
        //     focus,
        //     Focus::Workbench
        //         | Focus::Palette
        //         | Focus::Panel(crate::panel::kind::PanelKind::Plugin)
        //         | Focus::Panel(crate::panel::kind::PanelKind::Search)
        //         | Focus::Panel(crate::panel::kind::PanelKind::SourceControl)
        // ) {
        //     return true;
        // }
        //
        // if _keyboard_focus.get_untracked().is_some() {
        //     return true;
        // }
        false
    }
}

// fn extra_styles_for_range(
//     text_layout: &TextLayout,
//     start: usize,
//     end: usize,
//     bg_color: Option<Color>,
//     under_line: Option<Color>,
//     wave_line: Option<Color>,
// ) -> impl Iterator<Item = LineExtraStyle> + '_ {
//     let start_hit = text_layout.hit_position(start);
//     let end_hit = text_layout.hit_position(end);
//
//     // tracing::info!("start={start_hit:?} end={end_hit:?}");
//     text_layout
//         .layout_runs()
//         .enumerate()
//         .filter_map(move |(current_line, run)| {
//             if current_line < start_hit.line || current_line > end_hit.line {
//                 return None;
//             }
//
//             let x = if current_line == start_hit.line {
//                 start_hit.point.x
//             } else {
//                 run.glyphs.first().map(|g| g.x).unwrap_or(0.0) as f64
//             };
//             let end_x = if current_line == end_hit.line {
//                 end_hit.point.x
//             } else {
//                 run.glyphs.last().map(|g| g.x + g.w).unwrap_or(0.0) as f64
//             };
//             let width = end_x - x;
//
//             if width == 0.0 {
//                 return None;
//             }
//
//             let height = (run.max_ascent + run.max_descent) as f64;
//             let y = run.line_y as f64 - run.max_ascent as f64;
//
//             Some(LineExtraStyle {
//                 x,
//                 y,
//                 width: Some(width),
//                 height,
//                 bg_color,
//                 under_line,
//                 wave_line,
//             })
//         })
// }
