use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Duration;

use crate::commands::agent::workflow::{ExecutorKind, Node, NodeOp, NodeStatus, Workflow};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use unicode_width::UnicodeWidthChar;

const HORIZONTAL_MARGIN: u16 = 2;
const BOTTOM_MARGIN: u16 = 1;
const PROMPT_MIN_CONTENT_ROWS: usize = 3;
const TAGLINE: &str = "efficient coding harness";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_SHA: &str = env!("HAYCUT_BUILD_SHA");

const ANSI_COMPACT: [(&str, &str); 3] = [
    ("██  ██  ▄▄▄  ▄▄ ▄▄ ", "▄█████ ▄▄ ▄▄ ▄▄▄▄▄▄ "),
    ("██████ ██▀██ ▀███▀ ", "██     ██ ██   ██   "),
    ("██  ██ ██▀██   █   ", "▀█████ ▀███▀   ██  "),
];
const LOGO_CANVAS_HEIGHT: u16 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LandingVariant {
    Full,
    Compact,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayoutMode {
    Landing,
    Chat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveTab {
    Chat,
    Workflow,
}

fn prompt_rect_for(area: Rect, content_rows: usize) -> Rect {
    let margin = if area.width >= HORIZONTAL_MARGIN * 2 + 1 {
        HORIZONTAL_MARGIN
    } else {
        0
    };
    let width = area.width.saturating_sub(margin * 2).max(1);
    let available_height = area.height.saturating_sub(BOTTOM_MARGIN);
    let preferred_height = content_rows.max(PROMPT_MIN_CONTENT_ROWS).saturating_add(2) as u16;
    let height = preferred_height
        .min((area.height / 2).max(1))
        .min(available_height.max(1));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(BOTTOM_MARGIN + height));
    Rect::new(x, y, width, height)
}

fn landing_variant(area: Rect) -> LandingVariant {
    let full_width = ANSI_COMPACT
        .iter()
        .map(|(hay, cut)| hay.chars().count() + cut.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let full_height = LOGO_CANVAS_HEIGHT + 1;
    let compact_width = "HayCut".chars().count() as u16;
    if area.height < 2 || area.width < compact_width {
        LandingVariant::Hidden
    } else if area.width >= full_width && area.height >= full_height {
        LandingVariant::Full
    } else {
        LandingVariant::Compact
    }
}

fn ansi_logo_lines() -> Vec<Line<'static>> {
    let hay_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let cut_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    ANSI_COMPACT
        .iter()
        .map(|(hay, cut)| {
            Line::from(vec![
                Span::styled(*hay, hay_style),
                Span::styled(*cut, cut_style),
            ])
        })
        .collect()
}

fn render_branding(area: Rect, frame: &mut ratatui::Frame) {
    if area.height == 0 {
        return;
    }
    let metadata_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    frame.render_widget(
        Paragraph::new(Span::styled(metadata(true), metadata_style)).alignment(Alignment::Right),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let content_area = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    let variant = landing_variant(content_area);
    if variant == LandingVariant::Hidden {
        return;
    }
    let tagline_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    match variant {
        LandingVariant::Full => {
            let branding_height = LOGO_CANVAS_HEIGHT + 1;
            let branding_y =
                content_area.y + content_area.height.saturating_sub(branding_height) / 2;
            frame.render_widget(
                Paragraph::new(ansi_logo_lines()).alignment(Alignment::Center),
                Rect::new(
                    content_area.x,
                    branding_y,
                    content_area.width,
                    LOGO_CANVAS_HEIGHT,
                ),
            );
            frame.render_widget(
                Paragraph::new(Span::styled(TAGLINE, tagline_style)).alignment(Alignment::Center),
                Rect::new(
                    content_area.x,
                    branding_y + LOGO_CANVAS_HEIGHT,
                    content_area.width,
                    1,
                ),
            );
        }
        LandingVariant::Compact => {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "HayCut",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
                .alignment(Alignment::Center),
                Rect::new(content_area.x, content_area.y, content_area.width, 1),
            );
            frame.render_widget(
                Paragraph::new(Span::styled(TAGLINE, tagline_style)).alignment(Alignment::Center),
                Rect::new(content_area.x, content_area.y + 1, content_area.width, 1),
            );
        }
        LandingVariant::Hidden => unreachable!(),
    }
}

fn metadata(include_sha: bool) -> String {
    if include_sha {
        format!("v{VERSION} - {BUILD_SHA}")
    } else {
        format!("v{VERSION}")
    }
}

fn render_header(
    area: Rect,
    unseen_events: usize,
    active_tab: ActiveTab,
    frame: &mut ratatui::Frame,
) {
    if area.height == 0 {
        return;
    }
    let full_width = (6 + 1 + metadata(true).chars().count()) as u16;
    let right = metadata(area.width >= full_width);
    let gap = area
        .width
        .saturating_sub((6 + right.chars().count()) as u16) as usize;
    let hay_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let cut_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    let line = Line::from(vec![
        Span::styled("Hay", hay_style),
        Span::styled("Cut", cut_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(
            right,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if area.height > 1 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " Chat ",
                    if active_tab == ActiveTab::Chat {
                        hay_tab_style()
                    } else {
                        dim_tab_style()
                    },
                ),
                Span::styled(
                    " Workflow ",
                    if active_tab == ActiveTab::Workflow {
                        hay_tab_style()
                    } else {
                        dim_tab_style()
                    },
                ),
                Span::styled(
                    if unseen_events > 0 {
                        format!("  ↓ {} new", unseen_events)
                    } else {
                        String::new()
                    },
                    dim_tab_style(),
                ),
                Span::styled(
                    "─".repeat(area.width.saturating_sub(17) as usize),
                    dim_tab_style(),
                ),
            ])),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
}

fn hay_tab_style() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

fn dim_tab_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM)
}

pub fn run() -> i32 {
    match ratatui::run(|terminal| -> io::Result<()> {
        let mut stdout = io::stdout();
        let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
        execute!(stdout, EnableMouseCapture)?;
        if enhanced {
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                )
            )?;
        }

        let result = run_editor(terminal);
        let restore = if enhanced {
            execute!(stdout, PopKeyboardEnhancementFlags, DisableMouseCapture)
        } else {
            execute!(stdout, DisableMouseCapture)
        };
        result.and(restore)
    }) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Terminal error: {error}");
            1
        }
    }
}

fn run_editor(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    let mut app = App::default();
    terminal.draw(|frame| app.render(frame.area(), frame))?;

    loop {
        let event = if app.pending.is_some() {
            if event::poll(Duration::from_millis(120))? {
                event::read()?
            } else {
                app.tick();
                terminal.draw(|frame| app.render(frame.area(), frame))?;
                continue;
            }
        } else {
            event::read()?
        };
        if should_quit(event.clone()) {
            return Ok(());
        }
        if app.handle_event(event) {
            terminal.draw(|frame| app.render(frame.area(), frame))?;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TimelineEntry {
    User(String),
    Assistant(String),
    Workflow(WorkflowInstance),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum UiEvent {
    UserSubmitted {
        id: usize,
        text: String,
    },
    WorkflowStarted {
        id: usize,
    },
    NodeStarted {
        workflow_id: usize,
        node_id: usize,
    },
    NodeCompleted {
        workflow_id: usize,
        node_id: usize,
        outcome: Option<&'static str>,
    },
    NodeFailed {
        workflow_id: usize,
        node_id: usize,
        outcome: &'static str,
    },
    AssistantResponse {
        id: usize,
        text: String,
    },
    ContextSnapshot(ContextSidebarSnapshot),
    AssistantDelta {
        id: usize,
        text: String,
    },
    ModelStream(ModelStreamEvent),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UsageValue {
    Unknown,
    Estimated(usize),
    Reported(usize),
    LowerBound(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum ModelStreamEvent {
    CallStarted {
        id: String,
        purpose: String,
        input: UsageValue,
    },
    TextDelta {
        id: String,
        text: String,
    },
    ToolCallDelta {
        id: String,
        text: String,
    },
    UsageUpdate {
        id: String,
        input: UsageValue,
        output: UsageValue,
        cached_input: UsageValue,
    },
    CallCompleted {
        id: String,
        input: UsageValue,
        output: UsageValue,
        cached_input: UsageValue,
    },
    CallFailed {
        id: String,
    },
    CallCancelled {
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelCallSnapshot {
    id: String,
    purpose: String,
    input: UsageValue,
    output: UsageValue,
    cached_input: UsageValue,
    active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelMeterSnapshot {
    active: Option<ModelCallSnapshot>,
    input_total: UsageValue,
    output_total: UsageValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ContextSource {
    reference_id: String,
    category: String,
    tokens: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ContextSidebarSnapshot {
    revision: usize,
    goal: String,
    intent: String,
    project: String,
    packet_tokens: usize,
    packet_soft_limit: usize,
    packet_hard_limit: usize,
    raw_tokens_avoided: usize,
    selected_items: usize,
    available_items: usize,
    compiled_context_tokens: usize,
    sources: Vec<ContextSource>,
    model: String,
    model_purpose: String,
    model_calls: usize,
    input_tokens: usize,
    cached_input_tokens: usize,
    output_tokens: usize,
    active_node: String,
    completed_nodes: usize,
    failed_nodes: usize,
    retries: usize,
    verification: String,
}

impl Default for ContextSidebarSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            goal: "No task submitted".into(),
            intent: "pending".into(),
            project: "pending".into(),
            packet_tokens: 0,
            packet_soft_limit: 20_000,
            packet_hard_limit: 40_000,
            raw_tokens_avoided: 0,
            selected_items: 0,
            available_items: 0,
            compiled_context_tokens: 0,
            sources: Vec::new(),
            model: "—".into(),
            model_purpose: "—".into(),
            model_calls: 0,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            active_node: "—".into(),
            completed_nodes: 0,
            failed_nodes: 0,
            retries: 0,
            verification: "pending".into(),
        }
    }
}

#[derive(Default)]
struct SidebarCache {
    revision: usize,
    width: usize,
    collapsed_revision: usize,
    rows: Vec<RenderedRow>,
    hit_regions: Vec<HitRegion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RenderedRow {
    key: String,
    owner: String,
    local_row: usize,
    line: Line<'static>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum HitTarget {
    ToggleDetails { item_id: String },
    OpenReference { reference_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HitRegion {
    rect: Rect,
    target: HitTarget,
}

fn hit_test(regions: &[HitRegion], column: u16, row: u16) -> Option<HitTarget> {
    regions
        .iter()
        .find(|region| region.rect.contains((column, row).into()))
        .map(|region| region.target.clone())
}

#[derive(Default)]
struct RenderCache {
    event_revision: usize,
    width: usize,
    expansion_revision: usize,
    rows: Vec<RenderedRow>,
    row_index: HashMap<String, usize>,
    hit_regions: Vec<HitRegion>,
}

#[derive(Default)]
struct ChatViewport {
    anchor: Option<String>,
    offset: usize,
    follow_tail: bool,
    unseen_events: usize,
    height: usize,
}

impl ChatViewport {
    fn new() -> Self {
        Self {
            follow_tail: true,
            ..Self::default()
        }
    }
    fn max_offset(&self, total: usize) -> usize {
        total.saturating_sub(self.height.max(1))
    }
    fn set_offset(&mut self, offset: usize, total: usize) {
        self.offset = offset.min(self.max_offset(total));
        self.follow_tail = self.offset == self.max_offset(total);
        if self.follow_tail {
            self.unseen_events = 0;
        }
    }
    fn scroll_by(&mut self, delta: isize, total: usize) {
        let next = if delta.is_negative() {
            self.offset.saturating_sub(delta.unsigned_abs())
        } else {
            self.offset.saturating_add(delta as usize)
        };
        self.set_offset(next, total);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DemoEvent {
    NodeStarted(usize),
    NodeCompleted(usize, Option<&'static str>),
    NodeFailed(usize, &'static str),
    FinalResponse,
}

#[derive(Clone, Debug)]
struct WorkflowInstance {
    workflow: Workflow,
}

impl PartialEq for WorkflowInstance {
    fn eq(&self, other: &Self) -> bool {
        format!("{:?}", self.workflow) == format!("{:?}", other.workflow)
    }
}
impl Eq for WorkflowInstance {}

const NO_DEPS: &[&str] = &[];
const D_CLASSIFY: &[&str] = &["classify"];
const D_PROJECT: &[&str] = &["project"];
const D_VERIFY: &[&str] = &["verify"];
const D_BASELINE: &[&str] = &["baseline"];
const D_CONTEXT: &[&str] = &["context"];
const D_PLAN: &[&str] = &["source", "tests"];
const D_APPLY: &[&str] = &["plan"];
const D_FINAL_1: &[&str] = &["final-1"];
const D_RETRY: &[&str] = &["retry"];
const D_FINAL_2: &[&str] = &["final-2"];

fn workflow_spec() -> Workflow {
    let nodes = [
        (
            "classify",
            NO_DEPS,
            "Classify intent",
            Some("implement feature"),
        ),
        (
            "project",
            D_CLASSIFY,
            "Detect project",
            Some("Rust · cargo"),
        ),
        (
            "verify",
            D_PROJECT,
            "Resolve verification",
            Some("cargo test --bin haycut"),
        ),
        (
            "baseline",
            D_VERIFY,
            "Run baseline",
            Some("184 tests passed"),
        ),
        ("context", D_BASELINE, "Select context", None),
        ("source", D_CONTEXT, "Read TUI source", None),
        ("tests", D_CONTEXT, "Read TUI tests", None),
        ("plan", D_PLAN, "Plan patch", Some("2 files selected")),
        ("apply", D_APPLY, "Apply patch", Some("2 files changed")),
        (
            "final-1",
            D_APPLY,
            "Run final verification",
            Some("format check failed"),
        ),
        (
            "retry",
            D_FINAL_1,
            "Retry fix",
            Some("formatted TUI source"),
        ),
        (
            "final-2",
            D_RETRY,
            "Run final verification",
            Some("185 tests passed"),
        ),
        ("report", D_FINAL_2, "Report", None),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (id, dependencies, _label, outcome))| Node {
        id: id.to_string(),
        op: match index {
            0 => NodeOp::ClassifyIntent,
            1 => NodeOp::DetectProject,
            2 => NodeOp::ResolveVerification,
            3 => NodeOp::RunBaseline,
            4 => NodeOp::SelectContext,
            5 | 6 => NodeOp::ReadContext,
            7 => NodeOp::PlanPatch,
            8 => NodeOp::ApplyPatch,
            9 | 11 => NodeOp::RunFinalVerification,
            10 => NodeOp::RetryFix,
            12 => NodeOp::Report,
            _ => unreachable!(),
        },
        depends_on: dependencies
            .iter()
            .map(|dependency| dependency.to_string())
            .collect(),
        status: NodeStatus::Pending,
        produced_by: None,
        outcome: outcome.map(str::to_string),
    })
    .collect::<Vec<_>>();
    Workflow { nodes, seq: 0 }
}

fn scripted_outcome(id: &str) -> Option<&'static str> {
    match id {
        "classify" => Some("implement feature"),
        "project" => Some("Rust · cargo"),
        "verify" => Some("cargo test --bin haycut"),
        "baseline" => Some("184 tests passed"),
        "plan" => Some("2 files selected"),
        "apply" => Some("2 files changed"),
        "final-1" => Some("format check failed"),
        "retry" => Some("formatted TUI source"),
        "final-2" => Some("185 tests passed"),
        _ => None,
    }
}

impl WorkflowInstance {
    fn new() -> Self {
        Self {
            workflow: workflow_spec(),
        }
    }
    fn node_mut(&mut self, id: usize) -> Option<&mut Node> {
        self.workflow.nodes.get_mut(id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectedNode {
    id: String,
    dependencies: Vec<String>,
    label: String,
    status: NodeStatus,
    executor: ExecutorKind,
    outcome: Option<String>,
    produced_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkflowProjection {
    nodes: Vec<ProjectedNode>,
}

fn human_node_label(node: &Node) -> String {
    match node.id.as_str() {
        "source" => "Read TUI source".into(),
        "tests" => "Read TUI tests".into(),
        _ => node
            .op
            .name()
            .split('_')
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map_or(String::new(), |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn workflow_projection(workflow: &Workflow) -> WorkflowProjection {
    WorkflowProjection {
        nodes: workflow
            .nodes
            .iter()
            .map(|node| ProjectedNode {
                id: node.id.clone(),
                dependencies: node.depends_on.clone(),
                label: human_node_label(node),
                status: node.status,
                executor: node.op.executor(),
                outcome: node.outcome.clone(),
                produced_by: node.produced_by.clone(),
            })
            .collect(),
    }
}

fn reduce_events(events: &[UiEvent]) -> Vec<TimelineEntry> {
    let mut document = Vec::new();
    for event in events {
        match event {
            UiEvent::UserSubmitted { text, .. } => document.push(TimelineEntry::User(text.clone())),
            UiEvent::WorkflowStarted { .. } => {
                document.push(TimelineEntry::Workflow(WorkflowInstance::new()));
            }
            UiEvent::NodeStarted { node_id, .. } => {
                if let Some(TimelineEntry::Workflow(workflow)) = document
                    .iter_mut()
                    .rev()
                    .find(|entry| matches!(entry, TimelineEntry::Workflow(_)))
                {
                    workflow.node_mut(*node_id).unwrap().status = NodeStatus::Running;
                }
            }
            UiEvent::NodeCompleted {
                node_id, outcome, ..
            } => {
                if let Some(TimelineEntry::Workflow(workflow)) = document
                    .iter_mut()
                    .rev()
                    .find(|entry| matches!(entry, TimelineEntry::Workflow(_)))
                {
                    let node = workflow.node_mut(*node_id).unwrap();
                    node.status = NodeStatus::Done;
                    node.outcome = outcome.map(str::to_string);
                }
            }
            UiEvent::NodeFailed {
                node_id, outcome, ..
            } => {
                if let Some(TimelineEntry::Workflow(workflow)) = document
                    .iter_mut()
                    .rev()
                    .find(|entry| matches!(entry, TimelineEntry::Workflow(_)))
                {
                    let node = workflow.node_mut(*node_id).unwrap();
                    node.status = NodeStatus::Failed;
                    node.outcome = Some((*outcome).to_string());
                }
            }
            UiEvent::AssistantResponse { text, .. } => {
                document.push(TimelineEntry::Assistant(text.clone()))
            }
            UiEvent::AssistantDelta { text, .. } => {
                if let Some(TimelineEntry::Assistant(existing)) = document.last_mut() {
                    existing.push_str(text);
                } else {
                    document.push(TimelineEntry::Assistant(text.clone()));
                }
            }
            UiEvent::ModelStream(_) => {}
            UiEvent::ContextSnapshot(_) => {}
        }
    }
    document
}

fn latest_context_snapshot(events: &[UiEvent]) -> ContextSidebarSnapshot {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            UiEvent::ContextSnapshot(snapshot) => Some(snapshot.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn merge_usage(old: UsageValue, new: UsageValue) -> UsageValue {
    match (old, new) {
        (_, UsageValue::Reported(value)) => UsageValue::Reported(value),
        (UsageValue::Reported(value), _) => UsageValue::Reported(value),
        (UsageValue::LowerBound(old), UsageValue::Estimated(new)) => {
            UsageValue::LowerBound(old.max(new))
        }
        (UsageValue::Estimated(old), UsageValue::Estimated(new)) => {
            UsageValue::Estimated(old.max(new))
        }
        (UsageValue::LowerBound(old), UsageValue::LowerBound(new)) => {
            UsageValue::LowerBound(old.max(new))
        }
        (UsageValue::Unknown, value) | (value, UsageValue::Unknown) => value,
        (value, _) => value,
    }
}

fn add_usage(total: UsageValue, value: UsageValue) -> UsageValue {
    match (total, value) {
        (UsageValue::Unknown, UsageValue::Unknown) => UsageValue::Unknown,
        (UsageValue::Unknown, UsageValue::Reported(value))
        | (UsageValue::Reported(value), UsageValue::Unknown) => UsageValue::LowerBound(value),
        (UsageValue::Unknown, UsageValue::Estimated(value))
        | (UsageValue::Estimated(value), UsageValue::Unknown) => UsageValue::LowerBound(value),
        (UsageValue::Unknown, UsageValue::LowerBound(value))
        | (UsageValue::LowerBound(value), UsageValue::Unknown) => UsageValue::LowerBound(value),
        (UsageValue::Estimated(a), UsageValue::Estimated(b)) => UsageValue::Estimated(a + b),
        (UsageValue::Reported(a), UsageValue::Reported(b)) => UsageValue::Reported(a + b),
        (UsageValue::LowerBound(a), UsageValue::LowerBound(b)) => UsageValue::LowerBound(a + b),
        (UsageValue::Reported(a), UsageValue::Estimated(b))
        | (UsageValue::Estimated(b), UsageValue::Reported(a)) => UsageValue::Estimated(a + b),
        (UsageValue::Reported(a), UsageValue::LowerBound(b))
        | (UsageValue::LowerBound(b), UsageValue::Reported(a)) => UsageValue::LowerBound(a + b),
        (UsageValue::Estimated(a), UsageValue::LowerBound(b))
        | (UsageValue::LowerBound(b), UsageValue::Estimated(a)) => UsageValue::LowerBound(a + b),
    }
}

fn model_meter(events: &[UiEvent]) -> ModelMeterSnapshot {
    let mut calls: HashMap<String, ModelCallSnapshot> = HashMap::new();
    for event in events {
        let UiEvent::ModelStream(stream) = event else {
            continue;
        };
        match stream {
            ModelStreamEvent::CallStarted { id, purpose, input } => {
                calls.insert(
                    id.clone(),
                    ModelCallSnapshot {
                        id: id.clone(),
                        purpose: purpose.clone(),
                        input: *input,
                        output: UsageValue::Unknown,
                        cached_input: UsageValue::Unknown,
                        active: true,
                    },
                );
            }
            ModelStreamEvent::UsageUpdate {
                id,
                input,
                output,
                cached_input,
            }
            | ModelStreamEvent::CallCompleted {
                id,
                input,
                output,
                cached_input,
            } => {
                if let Some(call) = calls.get_mut(id) {
                    call.input = merge_usage(call.input, *input);
                    call.output = merge_usage(call.output, *output);
                    call.cached_input = merge_usage(call.cached_input, *cached_input);
                    if matches!(stream, ModelStreamEvent::CallCompleted { .. }) {
                        call.active = false;
                    }
                }
            }
            ModelStreamEvent::CallFailed { id } | ModelStreamEvent::CallCancelled { id } => {
                if let Some(call) = calls.get_mut(id) {
                    call.active = false;
                }
            }
            ModelStreamEvent::TextDelta { .. } | ModelStreamEvent::ToolCallDelta { .. } => {}
        }
    }
    let active = calls.values().find(|call| call.active).cloned();
    let mut input_total = UsageValue::Unknown;
    let mut output_total = UsageValue::Unknown;
    for call in calls.values().filter(|call| !call.active) {
        input_total = add_usage(input_total, call.input);
        output_total = add_usage(output_total, call.output);
    }
    ModelMeterSnapshot {
        active,
        input_total,
        output_total,
    }
}

fn format_usage(value: UsageValue) -> String {
    let (prefix, number) = match value {
        UsageValue::Unknown => return "—".into(),
        UsageValue::Estimated(value) => ("≈", value),
        UsageValue::Reported(value) => ("", value),
        UsageValue::LowerBound(value) => ("≥", value),
    };
    if number >= 1000 {
        format!("{prefix}{:.1}k", number as f64 / 1000.0)
    } else {
        format!("{prefix}{number}")
    }
}

fn model_call_for_node(node_id: usize) -> Option<(&'static str, &'static str, usize)> {
    match node_id {
        0 => Some(("classifying intent", "weak", 900)),
        7 => Some(("planning patch", "strong", 2_300)),
        12 => Some(("writing report", "strong", 1_600)),
        _ => None,
    }
}

#[derive(Default)]
struct DemoDriver {
    next_node: usize,
}

impl DemoDriver {
    fn start(&mut self) -> DemoEvent {
        self.next_node = 0;
        DemoEvent::NodeStarted(0)
    }

    fn advance(&mut self) -> Option<DemoEvent> {
        if self.next_node >= workflow_spec().nodes.len() {
            return None;
        }
        let current = self.next_node;
        self.next_node += 1;
        let workflow = workflow_spec();
        let node = &workflow.nodes[current];
        let completion = if node.id == "final-1" {
            DemoEvent::NodeFailed(current, scripted_outcome(&node.id).unwrap())
        } else {
            DemoEvent::NodeCompleted(current, scripted_outcome(&node.id))
        };
        Some(completion)
    }

    fn start_next(&self) -> Option<DemoEvent> {
        (self.next_node < workflow_spec().nodes.len())
            .then_some(DemoEvent::NodeStarted(self.next_node))
    }
}

struct InFlightTurn {
    animation_frame: usize,
}

struct App {
    layout: LayoutMode,
    active_tab: ActiveTab,
    editor: PromptEditor,
    events: Vec<UiEvent>,
    #[allow(dead_code)]
    timeline: Vec<TimelineEntry>,
    pending: Option<InFlightTurn>,
    #[allow(dead_code)]
    workflow: Option<WorkflowInstance>,
    demo: DemoDriver,
    cache: RenderCache,
    viewport: ChatViewport,
    workflow_cache: RenderCache,
    workflow_viewport: ChatViewport,
    selected_node: Option<usize>,
    context_snapshot: ContextSidebarSnapshot,
    context_open: bool,
    context_focus: bool,
    context_viewport: ChatViewport,
    context_cache: SidebarCache,
    collapsed_sections: HashSet<String>,
    collapsed_revision: usize,
    chat_area: Rect,
    workflow_area: Rect,
    context_area: Rect,
    terminal_width: u16,
    tab_regions: Vec<HitRegion>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            layout: LayoutMode::Landing,
            active_tab: ActiveTab::Chat,
            editor: PromptEditor::default(),
            events: Vec::new(),
            timeline: Vec::new(),
            pending: None,
            workflow: None,
            demo: DemoDriver::default(),
            cache: RenderCache::default(),
            viewport: ChatViewport::new(),
            workflow_cache: RenderCache::default(),
            workflow_viewport: ChatViewport::new(),
            selected_node: None,
            context_snapshot: ContextSidebarSnapshot::default(),
            context_open: false,
            context_focus: false,
            context_viewport: ChatViewport::new(),
            context_cache: SidebarCache::default(),
            collapsed_sections: HashSet::new(),
            collapsed_revision: 0,
            chat_area: Rect::default(),
            workflow_area: Rect::default(),
            context_area: Rect::default(),
            terminal_width: 0,
            tab_regions: Vec::new(),
        }
    }
}

impl App {
    fn handle_event(&mut self, event: Event) -> bool {
        if matches!(event, Event::Resize(_, _)) {
            return true;
        }
        if let Event::Mouse(mouse) = &event {
            if self.context_open && self.context_area.contains((mouse.column, mouse.row).into()) {
                if let Some(HitTarget::ToggleDetails { item_id }) =
                    hit_test(&self.context_cache.hit_regions, mouse.column, mouse.row)
                {
                    if item_id == "context-close" {
                        self.context_open = false;
                        self.context_focus = false;
                        return true;
                    }
                    if let Some(section) = item_id.strip_prefix("section:") {
                        if !self.collapsed_sections.insert(section.to_string()) {
                            self.collapsed_sections.remove(section);
                        }
                        self.collapsed_revision += 1;
                        return true;
                    }
                }
                self.context_focus = true;
                if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                ) {
                    self.context_viewport.scroll_by(
                        if mouse.kind == MouseEventKind::ScrollUp {
                            -3
                        } else {
                            3
                        },
                        self.context_cache.rows.len(),
                    );
                    return true;
                }
                return false;
            }
        }
        if let Event::Mouse(mouse) = event {
            if self.layout == LayoutMode::Chat {
                if let Some(HitTarget::ToggleDetails { item_id }) =
                    hit_test(&self.tab_regions, mouse.column, mouse.row)
                {
                    self.active_tab = if item_id == "chat" {
                        ActiveTab::Chat
                    } else {
                        ActiveTab::Workflow
                    };
                    return true;
                }
            }
            if self.chat_area.contains((mouse.column, mouse.row).into()) {
                let delta = match mouse.kind {
                    MouseEventKind::ScrollUp => -3,
                    MouseEventKind::ScrollDown => 3,
                    MouseEventKind::Down(MouseButton::Left) => {
                        let _ = hit_test(&self.cache.hit_regions, mouse.column, mouse.row);
                        return false;
                    }
                    _ => return false,
                };
                let total = self.cache.rows.len();
                self.viewport.scroll_by(delta, total);
                return true;
            }
            if self.active_tab == ActiveTab::Workflow
                && self
                    .workflow_area
                    .contains((mouse.column, mouse.row).into())
            {
                if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                ) {
                    let delta = if mouse.kind == MouseEventKind::ScrollUp {
                        -3
                    } else {
                        3
                    };
                    self.workflow_viewport
                        .scroll_by(delta, self.workflow_cache.rows.len());
                    return true;
                }
                if let Some(HitTarget::ToggleDetails { item_id }) =
                    hit_test(&self.workflow_cache.hit_regions, mouse.column, mouse.row)
                {
                    self.selected_node = self.workflow.as_ref().and_then(|workflow| {
                        workflow
                            .workflow
                            .nodes
                            .iter()
                            .position(|node| node.id == item_id)
                    });
                    return true;
                }
            }
            return false;
        }
        let Event::Key(key) = event else {
            return false;
        };
        if key.kind != crossterm::event::KeyEventKind::Press {
            return false;
        }
        if self.layout == LayoutMode::Chat
            && key.code == KeyCode::Char('b')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            self.context_open = !self.context_open;
            self.context_focus = self.context_open;
            return true;
        }
        if self.context_focus && self.context_open {
            let total = self.context_cache.rows.len();
            let page = self.context_viewport.height.saturating_sub(2).max(1) as isize;
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Home | KeyCode::End)
            {
                self.context_viewport
                    .set_offset(if key.code == KeyCode::Home { 0 } else { total }, total);
                return true;
            }
            if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                self.context_viewport.scroll_by(
                    if key.code == KeyCode::PageUp {
                        -page
                    } else {
                        page
                    },
                    total,
                );
                return true;
            }
        }
        let workflow_active = self.active_tab == ActiveTab::Workflow;
        let total = if workflow_active {
            self.workflow_cache.rows.len()
        } else {
            self.cache.rows.len()
        };
        let page = if workflow_active {
            self.workflow_viewport.height
        } else {
            self.viewport.height
        }
        .saturating_sub(2)
        .max(1) as isize;
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let delta = match key.code {
                KeyCode::Left | KeyCode::Right => {
                    self.active_tab = if self.active_tab == ActiveTab::Chat {
                        ActiveTab::Workflow
                    } else {
                        ActiveTab::Chat
                    };
                    return true;
                }
                KeyCode::Up => Some(-1),
                KeyCode::Down => Some(1),
                KeyCode::Home => {
                    if workflow_active {
                        self.workflow_viewport.set_offset(0, total);
                    } else {
                        self.viewport.set_offset(0, total);
                    }
                    return true;
                }
                KeyCode::End => {
                    if workflow_active {
                        self.workflow_viewport.set_offset(total, total);
                    } else {
                        self.viewport.set_offset(total, total);
                    }
                    return true;
                }
                _ => None,
            };
            if let Some(delta) = delta {
                if workflow_active {
                    self.workflow_viewport.scroll_by(delta, total);
                } else {
                    self.viewport.scroll_by(delta, total);
                }
                return true;
            }
        } else {
            let delta = match key.code {
                KeyCode::PageUp => Some(-page),
                KeyCode::PageDown => Some(page),
                _ => None,
            };
            if let Some(delta) = delta {
                if workflow_active {
                    self.workflow_viewport.scroll_by(delta, total);
                } else {
                    self.viewport.scroll_by(delta, total);
                }
                return true;
            }
        }
        if key.modifiers.contains(KeyModifiers::ALT) && self.active_tab == ActiveTab::Workflow {
            if matches!(key.code, KeyCode::Up | KeyCode::Down) {
                let count = self
                    .workflow
                    .as_ref()
                    .map_or(0, |workflow| workflow.workflow.nodes.len());
                if count > 0 {
                    let current = self.selected_node.unwrap_or(0);
                    self.selected_node = Some(if key.code == KeyCode::Up {
                        current.saturating_sub(1)
                    } else {
                        (current + 1).min(count - 1)
                    });
                    return true;
                }
            }
        }
        if key.code == KeyCode::Char('1')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
        {
            return self.advance_demo();
        }
        if key.code == KeyCode::Enter
            && !key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return self.submit();
        }
        self.editor.handle_event(Event::Key(key))
    }

    fn submit(&mut self) -> bool {
        if self.pending.is_some() {
            return false;
        }
        let prompt = self.editor.text();
        if prompt.trim().is_empty() {
            return false;
        }
        self.append_event(UiEvent::UserSubmitted {
            id: self.events.len(),
            text: prompt,
        });
        self.editor.reset();
        self.layout = LayoutMode::Chat;
        self.append_event(UiEvent::WorkflowStarted {
            id: self.events.len(),
        });
        let event = self.demo.start();
        self.apply_demo_event(event)
    }

    fn advance_demo(&mut self) -> bool {
        let Some(event) = self.demo.advance() else {
            return false;
        };
        let changed = self.apply_demo_event(event);
        if changed && self.pending.is_none() && self.demo.next_node < workflow_spec().nodes.len() {
            self.apply_demo_event(self.demo.start_next().unwrap())
        } else if changed && self.demo.next_node >= workflow_spec().nodes.len() {
            self.apply_demo_event(DemoEvent::FinalResponse)
        } else {
            changed
        }
    }

    fn apply_demo_event(&mut self, event: DemoEvent) -> bool {
        match event {
            DemoEvent::NodeStarted(id) => {
                self.append_event(UiEvent::NodeStarted {
                    workflow_id: 0,
                    node_id: id,
                });
                self.selected_node = Some(id);
                if let Some((purpose, tier, input)) = model_call_for_node(id) {
                    self.append_event(UiEvent::ModelStream(ModelStreamEvent::CallStarted {
                        id: format!("call-{id}"),
                        purpose: format!("{purpose} ({tier} model)"),
                        input: UsageValue::Estimated(input),
                    }));
                    if id == 12 {
                        self.append_event(UiEvent::AssistantResponse {
                            id: self.events.len(),
                            text: "Implemented the requested change.\n\n".into(),
                        });
                    }
                }
                self.pending = Some(InFlightTurn { animation_frame: 0 });
            }
            DemoEvent::NodeCompleted(id, outcome) => {
                self.append_event(UiEvent::NodeCompleted {
                    workflow_id: 0,
                    node_id: id,
                    outcome,
                });
                if let Some((_, _, input)) = model_call_for_node(id) {
                    let output = if id == 0 {
                        100
                    } else if id == 7 {
                        180
                    } else {
                        620
                    };
                    let cached = if id == 7 {
                        1_100
                    } else if id == 12 {
                        1_000
                    } else {
                        0
                    };
                    let reported = id == 12;
                    self.append_event(UiEvent::ModelStream(ModelStreamEvent::CallCompleted {
                        id: format!("call-{id}"),
                        input: if reported {
                            UsageValue::Reported(input)
                        } else {
                            UsageValue::Estimated(input)
                        },
                        output: if reported {
                            UsageValue::Reported(output)
                        } else {
                            UsageValue::Estimated(output)
                        },
                        cached_input: if cached == 0 {
                            UsageValue::Unknown
                        } else if reported {
                            UsageValue::Reported(cached)
                        } else {
                            UsageValue::Estimated(cached)
                        },
                    }));
                }
                self.pending = None;
            }
            DemoEvent::NodeFailed(id, outcome) => {
                self.append_event(UiEvent::NodeFailed {
                    workflow_id: 0,
                    node_id: id,
                    outcome,
                });
                if model_call_for_node(id).is_some() {
                    self.append_event(UiEvent::ModelStream(ModelStreamEvent::CallFailed {
                        id: format!("call-{id}"),
                    }));
                }
                self.pending = None;
            }
            DemoEvent::FinalResponse => {
                self.append_event(UiEvent::AssistantDelta { id: self.events.len(), text: "- Added incremental workflow-tree rendering\n- Added running, success, and failure states\n- Recovered from verification failure\n- 185 tests passed".into() });
                self.pending = None;
                self.selected_node = Some(12);
            }
        }
        self.append_event(UiEvent::ContextSnapshot(self.mock_context_snapshot()));
        true
    }

    fn mock_context_snapshot(&self) -> ContextSidebarSnapshot {
        let step = self.demo.next_node;
        let workflow = self.workflow.as_ref().map(|workflow| &workflow.workflow);
        let completed = workflow.map_or(0, |workflow| {
            workflow
                .nodes
                .iter()
                .filter(|node| node.status == NodeStatus::Done)
                .count()
        });
        let failed = workflow.map_or(0, |workflow| {
            workflow
                .nodes
                .iter()
                .filter(|node| node.status == NodeStatus::Failed)
                .count()
        });
        let active_node = workflow
            .and_then(|workflow| {
                workflow
                    .nodes
                    .iter()
                    .find(|node| node.status == NodeStatus::Running)
            })
            .map(|node| node.id.clone())
            .unwrap_or_else(|| "—".into());
        let final_state = step >= 13;
        let selected = if final_state {
            8
        } else if step >= 6 {
            6
        } else {
            0
        };
        let packet = if final_state {
            2_900
        } else if step >= 4 {
            720
        } else {
            0
        };
        let sources = (0..selected)
            .map(|index| ContextSource {
                reference_id: format!("ctx-{index:02}"),
                category: if index % 2 == 0 {
                    "source".into()
                } else {
                    "test".into()
                },
                tokens: if final_state { 600 } else { 300 },
            })
            .collect();
        ContextSidebarSnapshot {
            revision: self.events.len(),
            goal: self
                .timeline
                .iter()
                .find_map(|entry| match entry {
                    TimelineEntry::User(text) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "No task submitted".into()),
            intent: if step > 0 {
                "implement feature".into()
            } else {
                "pending".into()
            },
            project: if step > 1 {
                "Rust · cargo".into()
            } else {
                "pending".into()
            },
            packet_tokens: packet,
            packet_soft_limit: 20_000,
            packet_hard_limit: 40_000,
            raw_tokens_avoided: if final_state {
                28_600
            } else if step >= 4 {
                12_500
            } else {
                0
            },
            selected_items: selected,
            available_items: if selected > 0 { selected + 3 } else { 0 },
            compiled_context_tokens: if final_state {
                1_800
            } else if step >= 6 {
                1_800
            } else {
                0
            },
            sources,
            model: if step >= 7 {
                "strong model".into()
            } else {
                "weak model".into()
            },
            model_purpose: if step >= 10 {
                "patch recovery".into()
            } else {
                "workflow planning".into()
            },
            model_calls: if final_state {
                3
            } else if step >= 7 {
                1
            } else {
                0
            },
            input_tokens: if final_state {
                4_800
            } else if step >= 7 {
                2_300
            } else {
                0
            },
            cached_input_tokens: if final_state {
                2_100
            } else if step >= 7 {
                1_100
            } else {
                0
            },
            output_tokens: if final_state {
                620
            } else if step >= 7 {
                180
            } else {
                0
            },
            active_node,
            completed_nodes: completed,
            failed_nodes: failed,
            retries: usize::from(step >= 11),
            verification: if step >= 12 {
                "passed".into()
            } else if step >= 10 {
                "format check failed".into()
            } else {
                "pending".into()
            },
        }
    }

    fn append_event(&mut self, event: UiEvent) {
        self.events.push(event);
        self.context_snapshot = latest_context_snapshot(&self.events);
        self.timeline = reduce_events(&self.events);
        self.workflow = self.timeline.iter().rev().find_map(|entry| match entry {
            TimelineEntry::Workflow(workflow) => Some(workflow.clone()),
            _ => None,
        });
        self.cache.event_revision = usize::MAX;
        self.workflow_cache.event_revision = usize::MAX;
        self.context_cache.revision = usize::MAX;
        if !self.viewport.follow_tail {
            self.viewport.unseen_events += 1;
        }
    }

    fn tick(&mut self) -> bool {
        let Some(turn) = self.pending.as_mut() else {
            return false;
        };
        turn.animation_frame = (turn.animation_frame + 1) % SPINNER.len();
        let animation_frame = turn.animation_frame;
        let active = model_meter(&self.events).active;
        if let Some(call) = active {
            let output = match call.output {
                UsageValue::Estimated(value)
                | UsageValue::Reported(value)
                | UsageValue::LowerBound(value) => value + 8,
                UsageValue::Unknown => 8,
            };
            self.append_event(UiEvent::ModelStream(ModelStreamEvent::UsageUpdate {
                id: call.id,
                input: call.input,
                output: UsageValue::Estimated(output),
                cached_input: call.cached_input,
            }));
            if call.purpose.starts_with("writing report") {
                let chunks = [
                    "- Added incremental workflow-tree rendering\n",
                    "- Added running, success, and failure states\n",
                    "- Recovered from verification failure\n",
                    "- 185 tests passed",
                ];
                if let Some(chunk) = chunks.get(animation_frame.saturating_sub(1)) {
                    self.append_event(UiEvent::AssistantDelta {
                        id: self.events.len(),
                        text: (*chunk).into(),
                    });
                }
            }
        }
        true
    }

    fn render(&mut self, area: Rect, frame: &mut ratatui::Frame) {
        self.terminal_width = area.width;
        let prompt = prompt_rect_for(area, self.editor.desired_content_rows(area));
        match self.layout {
            LayoutMode::Landing => {
                let landing_height = prompt.y.saturating_sub(area.y + 1);
                let landing = Rect::new(area.x, area.y, area.width, landing_height);
                render_branding(landing, frame);
            }
            LayoutMode::Chat => {
                let header_height = area.height.min(2).min(prompt.y.saturating_sub(area.y));
                let header = Rect::new(area.x, area.y, area.width, header_height);
                self.tab_regions = vec![
                    HitRegion {
                        rect: Rect::new(area.x, area.y + 1, 7, 1),
                        target: HitTarget::ToggleDetails {
                            item_id: "chat".into(),
                        },
                    },
                    HitRegion {
                        rect: Rect::new(area.x + 7, area.y + 1, 11, 1),
                        target: HitTarget::ToggleDetails {
                            item_id: "workflow".into(),
                        },
                    },
                ];
                render_header(header, self.viewport.unseen_events, self.active_tab, frame);
                let viewport_y = header.y + header.height;
                let viewport = Rect::new(
                    area.x,
                    viewport_y,
                    area.width,
                    prompt.y.saturating_sub(viewport_y + 1),
                );
                let sidebar = self.context_open && context_supported(area.width, self.active_tab);
                let sidebar_width = if sidebar {
                    ((viewport.width as usize * 25 / 100).clamp(30, 42)) as u16
                } else {
                    0
                };
                let body = Rect::new(
                    viewport.x,
                    viewport.y,
                    viewport.width.saturating_sub(sidebar_width),
                    viewport.height,
                );
                self.context_area = if sidebar {
                    Rect::new(body.x + body.width, body.y, sidebar_width, body.height)
                } else {
                    Rect::default()
                };
                if self.active_tab == ActiveTab::Chat {
                    self.chat_area = body;
                    render_chat(
                        body,
                        &self.events,
                        &mut self.cache,
                        &mut self.viewport,
                        self.pending.as_ref(),
                        frame,
                    );
                } else {
                    self.chat_area = Rect::default();
                    self.workflow_area = body;
                    render_workflow_view(
                        body,
                        &self.events,
                        &mut self.workflow_cache,
                        &mut self.workflow_viewport,
                        self.selected_node,
                        self.pending.as_ref(),
                        frame,
                    );
                }
                if sidebar {
                    render_context_sidebar(
                        self.context_area,
                        &self.context_snapshot,
                        &mut self.context_cache,
                        &mut self.context_viewport,
                        &self.collapsed_sections,
                        self.collapsed_revision,
                        frame,
                    );
                }
            }
        }
        self.editor.render(
            prompt,
            &model_meter(&self.events),
            self.pending.as_ref().map_or(0, |turn| turn.animation_frame),
            frame,
        );
    }
}

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut row = String::new();
        let mut cells = 0;
        for ch in line.chars() {
            let char_width = ch.width().unwrap_or(0);
            if cells > 0 && cells + char_width > width {
                rows.push(std::mem::take(&mut row));
                cells = 0;
            }
            row.push(ch);
            cells += char_width;
        }
        rows.push(row);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn display_width(text: &str) -> usize {
    text.chars().map(|ch| ch.width().unwrap_or(0)).sum()
}

#[cfg(test)]
fn timeline_lines(
    entries: &[TimelineEntry],
    width: usize,
    pending: Option<&InFlightTurn>,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let user_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let assistant_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    for (index, entry) in entries.iter().enumerate() {
        let block = match entry {
            TimelineEntry::User(text) => user_block_lines(text, width, user_style),
            TimelineEntry::Assistant(text) => assistant_block_lines(text, width, assistant_style),
            TimelineEntry::Workflow(workflow) => workflow_block_lines(
                workflow,
                width,
                pending.map_or(0, |turn| turn.animation_frame),
            ),
        };
        lines.extend(block);
        if index + 1 < entries.len() {
            lines.push(Line::from(""));
        }
    }
    lines
}

fn workflow_block_lines(
    workflow: &WorkflowInstance,
    width: usize,
    animation_frame: usize,
) -> Vec<Line<'static>> {
    let heading_style = Style::default().add_modifier(Modifier::BOLD);
    let running_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let success_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let failure_style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    let connector_style = Style::default().fg(Color::DarkGray);
    let mut lines = vec![Line::from(vec![
        Span::styled("│ ", connector_style),
        Span::styled("Workflow", heading_style),
    ])];
    let projection = workflow_projection(&workflow.workflow);
    let visible: Vec<_> = projection
        .nodes
        .iter()
        .filter(|node| node.status != NodeStatus::Pending)
        .collect();
    let compact = width < 42;
    for (index, node) in visible.iter().enumerate() {
        let depth = node
            .dependencies
            .first()
            .and_then(|parent| {
                projection
                    .nodes
                    .iter()
                    .position(|candidate| candidate.id == *parent)
            })
            .map_or(0, |parent| {
                projection.nodes[..parent]
                    .iter()
                    .filter(|n| n.status != NodeStatus::Pending)
                    .count()
                    .min(3)
            });
        let indent = "  ".repeat(depth);
        let branch = if index + 1 == visible.len() {
            "└─"
        } else {
            "├─"
        };
        let (symbol, style) = match node.status {
            NodeStatus::Running => (SPINNER[animation_frame % SPINNER.len()], running_style),
            NodeStatus::Done => ("✓", success_style),
            NodeStatus::Failed => ("✗", failure_style),
            _ => ("·", connector_style),
        };
        let merge = if node.dependencies.len() > 1 {
            "  2 inputs"
        } else {
            ""
        };
        lines.push(Line::from(vec![
            Span::styled(format!("│ {indent}{branch} "), connector_style),
            Span::styled(
                format!(
                    "{symbol} {} [{}]{}",
                    node.label,
                    node.executor.name(),
                    merge
                ),
                style,
            ),
        ]));
        if let Some(outcome) = node.outcome.as_deref().filter(|_| !compact) {
            lines.extend(
                wrap_text(outcome, width.saturating_sub(depth * 2 + 8).max(1))
                    .into_iter()
                    .map(|text| {
                        Line::from(vec![
                            Span::styled(format!("│ {indent}   "), connector_style),
                            Span::styled(text, style),
                        ])
                    }),
            );
        }
    }
    lines
}

fn assistant_block_lines(text: &str, width: usize, role_style: Style) -> Vec<Line<'static>> {
    let content_width = width.saturating_sub(2).max(1);
    let mut lines = vec![Line::from(vec![
        Span::styled("╭ ", role_style),
        Span::styled("HayCut", role_style),
    ])];
    lines.extend(
        wrap_text(text, content_width)
            .into_iter()
            .map(|content| Line::from(vec![Span::styled("│ ", role_style), Span::raw(content)])),
    );
    lines.push(Line::from(Span::styled("╰", role_style)));
    lines
}

fn user_block_lines(text: &str, width: usize, role_style: Style) -> Vec<Line<'static>> {
    let content_width = width.saturating_sub(1).max(1);
    let left_padding = " ".repeat(width.saturating_sub(5));
    let mut lines = vec![Line::from(vec![
        Span::raw(left_padding.clone()),
        Span::styled("You ╮", role_style),
    ])];
    lines.extend(wrap_text(text, content_width).into_iter().map(|content| {
        let padding = " ".repeat(content_width.saturating_sub(display_width(&content)));
        Line::from(vec![
            Span::raw(" ".repeat(width.saturating_sub(content_width + 1))),
            Span::raw(content),
            Span::raw(padding),
            Span::styled("│", role_style),
        ])
    }));
    lines.push(Line::from(vec![
        Span::raw(" ".repeat(width.saturating_sub(1))),
        Span::styled("╯", role_style),
    ]));
    lines
}

fn render_chat(
    area: Rect,
    events: &[UiEvent],
    cache: &mut RenderCache,
    viewport: &mut ChatViewport,
    pending: Option<&InFlightTurn>,
    frame: &mut ratatui::Frame,
) {
    if area.height == 0 {
        return;
    }
    let margin = if area.width >= HORIZONTAL_MARGIN * 2 + 1 {
        HORIZONTAL_MARGIN
    } else {
        0
    };
    let chat = Rect::new(
        area.x + margin,
        area.y,
        area.width.saturating_sub(margin * 2),
        area.height,
    );
    if chat.width == 0 {
        return;
    }
    viewport.height = chat.height as usize;
    let width = chat.width as usize;
    let old_anchor = viewport.anchor.clone();
    if cache.event_revision != events.len() || cache.width != width || cache.expansion_revision != 0
    {
        let document = reduce_events(events);
        let rows = rendered_rows(&document, width, pending);
        cache.rows = rows;
        cache.row_index = cache
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.key.clone(), index))
            .collect();
        cache.hit_regions.clear();
        cache.event_revision = events.len();
        cache.width = width;
        cache.expansion_revision = 0;
        if viewport.follow_tail {
            viewport.offset = viewport.max_offset(cache.rows.len());
        } else if let Some(anchor) = old_anchor {
            viewport.offset = cache
                .rows
                .iter()
                .position(|row| row.owner == anchor)
                .unwrap_or(viewport.offset)
                .min(viewport.max_offset(cache.rows.len()));
        }
    }
    if let Some(turn) = pending {
        let document = reduce_events(events);
        if let Some((index, workflow)) = document.iter().enumerate().find_map(|(index, entry)| {
            matches!(entry, TimelineEntry::Workflow(_)).then(|| {
                let TimelineEntry::Workflow(workflow) = entry else {
                    unreachable!()
                };
                (index, workflow)
            })
        }) {
            let owner = format!("workflow-{index}");
            let running = workflow_block_lines(workflow, width, turn.animation_frame);
            for (row, line) in cache
                .rows
                .iter_mut()
                .filter(|row| row.owner == owner)
                .zip(running)
            {
                row.line = line;
            }
        }
    }
    viewport.offset = viewport.offset.min(viewport.max_offset(cache.rows.len()));
    viewport.follow_tail = viewport.offset == viewport.max_offset(cache.rows.len());
    if viewport.follow_tail {
        viewport.unseen_events = 0;
    }
    viewport.anchor = cache.rows.get(viewport.offset).map(|row| row.owner.clone());
    let visible: Vec<Line<'static>> = cache
        .rows
        .iter()
        .skip(viewport.offset)
        .take(viewport.height)
        .map(|row| row.line.clone())
        .collect();
    frame.render_widget(Paragraph::new(visible), chat);
}

fn rendered_rows(
    entries: &[TimelineEntry],
    width: usize,
    pending: Option<&InFlightTurn>,
) -> Vec<RenderedRow> {
    let mut rows = Vec::new();
    let user_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let assistant_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    for (index, entry) in entries.iter().enumerate() {
        let (owner, block) = match entry {
            TimelineEntry::User(text) => (
                format!("user-{index}"),
                user_block_lines(text, width, user_style),
            ),
            TimelineEntry::Assistant(text) => (
                format!("assistant-{index}"),
                assistant_block_lines(text, width, assistant_style),
            ),
            TimelineEntry::Workflow(workflow) => (
                format!("workflow-{index}"),
                workflow_block_lines(
                    workflow,
                    width,
                    pending.map_or(0, |turn| turn.animation_frame),
                ),
            ),
        };
        for (local_row, line) in block.into_iter().enumerate() {
            rows.push(RenderedRow {
                key: format!("{owner}-{local_row}"),
                owner: owner.clone(),
                local_row,
                line,
            });
        }
        if index + 1 < entries.len() {
            rows.push(RenderedRow {
                key: format!("{owner}-gap"),
                owner: owner.clone(),
                local_row: usize::MAX,
                line: Line::from(""),
            });
        }
    }
    rows
}

fn workflow_graph_lines(
    workflow: &WorkflowInstance,
    width: usize,
    animation_frame: usize,
) -> Vec<RenderedRow> {
    let projection = workflow_projection(&workflow.workflow);
    let mut lines = vec![RenderedRow {
        key: "workflow-heading".into(),
        owner: "workflow".into(),
        local_row: 0,
        line: Line::from(Span::styled("Workflow graph", hay_tab_style())),
    }];
    let visible: Vec<_> = projection
        .nodes
        .iter()
        .filter(|node| node.status != NodeStatus::Pending)
        .collect();
    for (index, node) in visible.iter().enumerate() {
        let depth = node
            .dependencies
            .first()
            .and_then(|parent| {
                projection
                    .nodes
                    .iter()
                    .position(|candidate| candidate.id == *parent)
            })
            .map_or(0, |parent| {
                projection.nodes[..parent]
                    .iter()
                    .filter(|node| node.status != NodeStatus::Pending)
                    .count()
                    .min(3)
            });
        let style = match node.status {
            NodeStatus::Running => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            NodeStatus::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            NodeStatus::Done => dim_tab_style(),
            _ => Style::default(),
        };
        let symbol = match node.status {
            NodeStatus::Running => SPINNER[animation_frame % SPINNER.len()],
            NodeStatus::Failed => "✗",
            NodeStatus::Done => "✓",
            _ => "·",
        };
        let branch = if index + 1 == visible.len() {
            "└─"
        } else {
            "├─"
        };
        let text = format!(
            "{}{} {} [{}]  id={}",
            "  ".repeat(depth),
            branch,
            node.label,
            node.executor.name(),
            node.id
        );
        lines.push(RenderedRow {
            key: format!("node-{}", node.id),
            owner: node.id.clone(),
            local_row: 0,
            line: Line::from(vec![Span::styled(format!("{symbol} {text}"), style)]),
        });
        if let Some(outcome) = node.outcome.as_deref() {
            lines.push(RenderedRow {
                key: format!("node-{}-outcome", node.id),
                owner: node.id.clone(),
                local_row: 1,
                line: Line::from(Span::styled(format!("   outcome: {outcome}"), style)),
            });
        }
    }
    if lines.len() == 1 {
        lines.push(RenderedRow {
            key: "workflow-empty".into(),
            owner: "workflow".into(),
            local_row: 1,
            line: Line::from("waiting for workflow"),
        });
    }
    let _ = width;
    lines
}

fn render_workflow_view(
    area: Rect,
    events: &[UiEvent],
    cache: &mut RenderCache,
    viewport: &mut ChatViewport,
    selected: Option<usize>,
    pending: Option<&InFlightTurn>,
    frame: &mut ratatui::Frame,
) {
    let Some(workflow) = reduce_events(events)
        .into_iter()
        .rev()
        .find_map(|entry| match entry {
            TimelineEntry::Workflow(workflow) => Some(workflow),
            _ => None,
        })
    else {
        return;
    };
    let graph_width = if area.width >= 80 {
        area.width * 65 / 100
    } else {
        area.width
    };
    let inspector_width = area
        .width
        .saturating_sub(graph_width + if area.width >= 80 { 1 } else { 0 });
    let graph_height = if area.width >= 80 {
        area.height
    } else {
        area.height.saturating_mul(2) / 3
    };
    viewport.height = graph_height as usize;
    if cache.event_revision != events.len() || cache.width != graph_width as usize {
        cache.rows = workflow_graph_lines(
            &workflow,
            graph_width as usize,
            pending.map_or(0, |turn| turn.animation_frame),
        );
        cache.event_revision = events.len();
        cache.width = graph_width as usize;
        viewport.offset = if viewport.follow_tail {
            viewport.max_offset(cache.rows.len())
        } else {
            viewport.offset.min(viewport.max_offset(cache.rows.len()))
        };
    }
    cache.hit_regions = cache
        .rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            (index >= viewport.offset
                && index < viewport.offset + viewport.height
                && row.local_row == 0
                && row.owner != "workflow")
                .then(|| HitRegion {
                    rect: Rect::new(
                        area.x,
                        area.y + (index - viewport.offset) as u16,
                        graph_width,
                        1,
                    ),
                    target: HitTarget::ToggleDetails {
                        item_id: row.owner.clone(),
                    },
                })
        })
        .collect();
    let visible: Vec<Line<'static>> = cache
        .rows
        .iter()
        .skip(viewport.offset)
        .take(viewport.height.max(1))
        .map(|row| row.line.clone())
        .collect();
    frame.render_widget(
        Paragraph::new(visible),
        Rect::new(area.x, area.y, graph_width, graph_height),
    );
    if inspector_width > 0 || area.width < 80 {
        let stacked = area.width < 80;
        let x = if stacked {
            area.x
        } else {
            area.x + graph_width + 1
        };
        let y = if stacked {
            area.y + graph_height
        } else {
            area.y
        };
        let height = if stacked {
            area.height.saturating_sub(graph_height)
        } else {
            area.height
        };
        if height == 0 {
            return;
        }
        let inspector = workflow_projection(&workflow.workflow)
            .nodes
            .get(selected.unwrap_or(0))
            .cloned();
        let lines = inspector.map_or_else(
            || vec![Line::from("No node selected")],
            |node| {
                vec![
                    Line::from(Span::styled("Node inspector", hay_tab_style())),
                    Line::from(format!("{} ({})", node.label, node.id)),
                    Line::from(format!("state: {:?}", node.status)),
                    Line::from(format!("executor: {:?}", node.executor)),
                    Line::from(format!("depends on: {:?}", node.dependencies)),
                    Line::from(format!("produced by: {:?}", node.produced_by)),
                    Line::from(format!(
                        "outcome: {}",
                        node.outcome.as_deref().unwrap_or("pending")
                    )),
                    Line::from("model/tool references: reserved"),
                ]
            },
        );
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(
                x,
                y,
                if stacked { area.width } else { inspector_width },
                height,
            ),
        );
    }
}

fn context_supported(width: u16, tab: ActiveTab) -> bool {
    width >= 96 || (width >= 70 && tab == ActiveTab::Chat)
}

fn budget_style(snapshot: &ContextSidebarSnapshot) -> Style {
    if snapshot.packet_tokens > snapshot.packet_hard_limit {
        Style::default().fg(Color::Red)
    } else if snapshot.packet_tokens > snapshot.packet_soft_limit {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    }
}

fn context_rows(
    snapshot: &ContextSidebarSnapshot,
    width: usize,
    collapsed: &HashSet<String>,
) -> (Vec<RenderedRow>, Vec<HitRegion>) {
    let label = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let value = Style::default();
    let mut rows = Vec::new();
    let mut hits = Vec::new();
    let mut add_section = |name: &str, content: Vec<String>| {
        let owner = format!("section:{name}");
        let header_index = rows.len();
        rows.push(RenderedRow {
            key: owner.clone(),
            owner: owner.clone(),
            local_row: 0,
            line: Line::from(vec![Span::styled(
                if collapsed.contains(name) {
                    format!("▸ {name}")
                } else {
                    format!("▾ {name}")
                },
                hay_tab_style(),
            )]),
        });
        hits.push(HitRegion {
            rect: Rect::new(0, header_index as u16, width as u16, 1),
            target: HitTarget::ToggleDetails { item_id: owner },
        });
        if !collapsed.contains(name) {
            for (index, text) in content.into_iter().enumerate() {
                rows.push(RenderedRow {
                    key: format!("{name}-{index}"),
                    owner: format!("section:{name}"),
                    local_row: index + 1,
                    line: Line::from(vec![Span::styled(text, value)]),
                });
            }
        }
        rows.push(RenderedRow {
            key: format!("{name}-gap"),
            owner: format!("section:{name}"),
            local_row: usize::MAX,
            line: Line::from(""),
        });
        let _ = &label;
    };
    let used = snapshot
        .packet_tokens
        .min(snapshot.packet_hard_limit.max(1));
    let bar_width = width.saturating_sub(10).max(4);
    let filled = bar_width * used / snapshot.packet_hard_limit.max(1);
    let bar = format!(
        "[{}{}]",
        "█".repeat(filled.min(bar_width)),
        "·".repeat(bar_width.saturating_sub(filled))
    );
    add_section(
        "Tokens",
        vec![
            format!("budget  {bar}"),
            format!(
                "used    {} / {} tokens",
                snapshot.packet_tokens, snapshot.packet_hard_limit
            ),
            format!("soft    {}", snapshot.packet_soft_limit),
            format!(
                "avoided {} ({:.1}%)",
                snapshot.raw_tokens_avoided,
                if snapshot.raw_tokens_avoided == 0 {
                    0.0
                } else {
                    100.0 * snapshot.raw_tokens_avoided as f64
                        / (snapshot.raw_tokens_avoided + snapshot.packet_tokens.max(1)) as f64
                }
            ),
            format!(
                "model   in {} · cache {} · out {}",
                snapshot.input_tokens, snapshot.cached_input_tokens, snapshot.output_tokens
            ),
        ],
    );
    let mut context_content = vec![
        format!(
            "items   {} / {}",
            snapshot.selected_items, snapshot.available_items
        ),
        format!("compiled {} tokens", snapshot.compiled_context_tokens),
    ];
    context_content.extend(snapshot.sources.iter().map(|source| {
        format!(
            "{}  {} · {}",
            source.reference_id, source.category, source.tokens
        )
    }));
    add_section("Selected context", context_content);
    add_section(
        "Model",
        vec![
            format!("tier    {}", snapshot.model),
            format!("purpose {}", snapshot.model_purpose),
            format!("calls   {}", snapshot.model_calls),
            format!(
                "usage   in {} · cache {} · out {}",
                snapshot.input_tokens, snapshot.cached_input_tokens, snapshot.output_tokens
            ),
        ],
    );
    add_section(
        "Task",
        vec![
            format!(
                "goal    {}",
                snapshot
                    .goal
                    .chars()
                    .take(width.saturating_sub(8))
                    .collect::<String>()
            ),
            format!("intent  {}", snapshot.intent),
            format!("project {}", snapshot.project),
            format!("active  {}", snapshot.active_node),
            format!(
                "nodes   {} done · {} failed",
                snapshot.completed_nodes, snapshot.failed_nodes
            ),
            format!(
                "retry   {} · verify {}",
                snapshot.retries, snapshot.verification
            ),
        ],
    );
    if let Some(start) = rows.iter().position(|row| row.key == "Selected context-1") {
        for (index, source) in snapshot.sources.iter().enumerate() {
            hits.push(HitRegion {
                rect: Rect::new(0, (start + 1 + index) as u16, width as u16, 1),
                target: HitTarget::OpenReference {
                    reference_id: source.reference_id.clone(),
                },
            });
        }
    }
    if let Some(row) = rows.iter_mut().find(|row| row.key == "section:Tokens-1") {
        row.line = Line::from(vec![
            Span::styled("used    ", label),
            Span::styled(
                format!(
                    "{} / {} tokens",
                    snapshot.packet_tokens, snapshot.packet_hard_limit
                ),
                budget_style(snapshot),
            ),
        ]);
    }
    (rows, hits)
}

fn render_context_sidebar(
    area: Rect,
    snapshot: &ContextSidebarSnapshot,
    cache: &mut SidebarCache,
    viewport: &mut ChatViewport,
    collapsed: &HashSet<String>,
    collapsed_revision: usize,
    frame: &mut ratatui::Frame,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    viewport.height = area.height as usize;
    if cache.revision != snapshot.revision
        || cache.width != area.width as usize
        || cache.collapsed_revision != collapsed_revision
    {
        let (rows, mut hits) =
            context_rows(snapshot, area.width.saturating_sub(1) as usize, collapsed);
        cache.rows = rows;
        cache.revision = snapshot.revision;
        cache.width = area.width as usize;
        cache.collapsed_revision = collapsed_revision;
        hits.iter_mut().for_each(|hit| hit.rect.x = area.x + 1);
        hits.iter_mut().for_each(|hit| {
            hit.rect.y = area.y
                + hit
                    .rect
                    .y
                    .saturating_sub(viewport.offset.min(u16::MAX as usize) as u16)
        });
        hits.push(HitRegion {
            rect: Rect::new(area.x + area.width.saturating_sub(3), area.y, 3, 1),
            target: HitTarget::ToggleDetails {
                item_id: "context-close".into(),
            },
        });
        cache.hit_regions = hits;
        viewport.offset = viewport.offset.min(viewport.max_offset(cache.rows.len()));
    }
    let lines: Vec<Line<'static>> = cache
        .rows
        .iter()
        .skip(viewport.offset)
        .take(viewport.height.max(1))
        .map(|row| row.line.clone())
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(ratatui::widgets::Borders::LEFT)
                .title("Context"),
        ),
        area,
    );
}

struct PromptEditor {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
    vertical_scroll: usize,
}

impl Default for PromptEditor {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            vertical_scroll: 0,
        }
    }
}

impl PromptEditor {
    fn desired_content_rows(&self, area: Rect) -> usize {
        let margin = if area.width >= HORIZONTAL_MARGIN * 2 + 1 {
            HORIZONTAL_MARGIN
        } else {
            0
        };
        let width = area
            .width
            .saturating_sub(margin * 2)
            .saturating_sub(4)
            .max(1) as usize;
        self.visual_rows(width).len().max(PROMPT_MIN_CONTENT_ROWS)
    }

    fn reset(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.vertical_scroll = 0;
    }

    fn handle_event(&mut self, event: Event) -> bool {
        let Event::Key(key) = event else {
            return matches!(event, Event::Resize(_, _));
        };
        if key.kind != crossterm::event::KeyEventKind::Press {
            return false;
        }
        match key.code {
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.insert(ch)
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => self.newline(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Up => self.up(),
            KeyCode::Down => self.down(),
            KeyCode::Home => {
                self.cursor_col = 0;
                true
            }
            KeyCode::End => {
                self.cursor_col = self.lines[self.cursor_line].chars().count();
                true
            }
            _ => false,
        }
    }

    fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn insert(&mut self, ch: char) -> bool {
        let byte = self.lines[self.cursor_line]
            .char_indices()
            .nth(self.cursor_col)
            .map_or(self.lines[self.cursor_line].len(), |(i, _)| i);
        self.lines[self.cursor_line].insert(byte, ch);
        self.cursor_col += 1;
        true
    }

    fn newline(&mut self) -> bool {
        let byte = self.lines[self.cursor_line]
            .char_indices()
            .nth(self.cursor_col)
            .map_or(self.lines[self.cursor_line].len(), |(i, _)| i);
        let rest = self.lines[self.cursor_line].split_off(byte);
        self.lines.insert(self.cursor_line + 1, rest);
        self.cursor_line += 1;
        self.cursor_col = 0;
        true
    }

    fn backspace(&mut self) -> bool {
        if self.cursor_col > 0 {
            let start = self.lines[self.cursor_line]
                .char_indices()
                .nth(self.cursor_col - 1)
                .map(|(i, _)| i)
                .unwrap();
            let end = self.lines[self.cursor_line]
                .char_indices()
                .nth(self.cursor_col)
                .map_or(self.lines[self.cursor_line].len(), |(i, _)| i);
            self.lines[self.cursor_line].replace_range(start..end, "");
            self.cursor_col -= 1;
            true
        } else if self.cursor_line > 0 {
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].chars().count();
            self.lines[self.cursor_line].push_str(&current);
            true
        } else {
            false
        }
    }

    fn delete(&mut self) -> bool {
        let len = self.lines[self.cursor_line].chars().count();
        if self.cursor_col < len {
            let _ = self.right();
            let _ = self.backspace();
            true
        } else if self.cursor_line + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
            true
        } else {
            false
        }
    }

    fn left(&mut self) -> bool {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            true
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].chars().count();
            true
        } else {
            false
        }
    }
    fn right(&mut self) -> bool {
        if self.cursor_col < self.lines[self.cursor_line].chars().count() {
            self.cursor_col += 1;
            true
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
            true
        } else {
            false
        }
    }
    fn up(&mut self) -> bool {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self
                .cursor_col
                .min(self.lines[self.cursor_line].chars().count());
            true
        } else {
            false
        }
    }
    fn down(&mut self) -> bool {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = self
                .cursor_col
                .min(self.lines[self.cursor_line].chars().count());
            true
        } else {
            false
        }
    }

    fn render(
        &mut self,
        prompt: Rect,
        meter: &ModelMeterSnapshot,
        animation_frame: usize,
        frame: &mut ratatui::Frame,
    ) {
        let Rect {
            x,
            y,
            width,
            height,
        } = prompt;
        let inner_width = width.saturating_sub(4).max(1) as usize;
        let rows = self.visual_rows(inner_width);
        let content_height = height.saturating_sub(2).max(1) as usize;
        let cursor = rows
            .iter()
            .position(|row| {
                row.0 == self.cursor_line && self.cursor_col >= row.1 && self.cursor_col <= row.2
            })
            .unwrap_or(0);
        if cursor < self.vertical_scroll {
            self.vertical_scroll = cursor;
        }
        if cursor >= self.vertical_scroll + content_height {
            self.vertical_scroll = cursor + 1 - content_height;
        }
        let visible: Vec<Line<'_>> = rows
            .iter()
            .skip(self.vertical_scroll)
            .take(content_height)
            .map(|row| Line::from(row.3.clone()))
            .collect();
        let block = Block::default().borders(Borders::ALL);
        frame.render_widget(
            Paragraph::new(visible).block(block.padding(Padding::horizontal(1))),
            Rect::new(x, y, width, height),
        );
        render_meter(prompt, meter, animation_frame, frame);
        let cursor_x = rows.get(cursor).map_or(0, |row| {
            row.3
                .chars()
                .take(self.cursor_col.saturating_sub(row.1))
                .map(|ch| ch.width().unwrap_or(0))
                .sum::<usize>()
        }) as u16;
        if width > 0 && height > 0 {
            let max_x = prompt.x + prompt.width.saturating_sub(1);
            let max_y = prompt.y + prompt.height.saturating_sub(1);
            frame.set_cursor_position((
                (x + 2 + cursor_x.min(inner_width as u16)).min(max_x),
                (y + 1 + cursor.saturating_sub(self.vertical_scroll) as u16).min(max_y),
            ));
        }
    }

    fn visual_rows(&self, width: usize) -> Vec<(usize, usize, usize, String)> {
        let mut rows = Vec::new();
        for (line_no, line) in self.lines.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                rows.push((line_no, 0, 0, String::new()));
                continue;
            }
            let mut start = 0;
            while start < chars.len() {
                let mut end = start;
                let mut cells = 0;
                while end < chars.len() {
                    let w = chars[end].width().unwrap_or(0);
                    if end > start && cells + w > width {
                        break;
                    }
                    cells += w;
                    end += 1;
                }
                rows.push((line_no, start, end, chars[start..end].iter().collect()));
                start = end;
            }
        }
        rows
    }
}

fn render_meter(
    prompt: Rect,
    meter: &ModelMeterSnapshot,
    animation_frame: usize,
    frame: &mut ratatui::Frame,
) {
    if prompt.width < 4 || prompt.height == 0 {
        return;
    }
    let width = prompt.width.saturating_sub(2) as usize;
    let dim = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let input_style = Style::default().fg(Color::Yellow);
    let output_style = Style::default().fg(Color::Green);
    let (left, input, output, active) = if let Some(call) = &meter.active {
        (
            format!(
                "{} {}",
                SPINNER[animation_frame % SPINNER.len()],
                call.purpose
            ),
            format_usage(call.input),
            format_usage(call.output),
            true,
        )
    } else {
        (
            String::new(),
            format_usage(meter.input_total),
            format_usage(meter.output_total),
            false,
        )
    };
    if !active && width < 24 {
        return;
    }
    let medium = width < 42;
    let left = if medium { String::new() } else { left };
    let prefix = if active { "" } else { "Σ " };
    let right = format!("{prefix}↑ {input}  ↓ {output}");
    if active && width < 16 {
        frame.render_widget(
            Paragraph::new(Span::styled(SPINNER[animation_frame % SPINNER.len()], dim)),
            Rect::new(prompt.x + 1, prompt.y + prompt.height - 1, 2, 1),
        );
        return;
    }
    let gap = width.saturating_sub(display_width(&left) + display_width(&right));
    let line = Line::from(vec![
        Span::styled(left, dim),
        Span::raw(" ".repeat(gap)),
        Span::styled(
            format!("{prefix}↑ {input}"),
            if active { input_style } else { dim },
        ),
        Span::styled(
            format!("  ↓ {output}"),
            if active { output_style } else { dim },
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line),
        Rect::new(
            prompt.x + 1,
            prompt.y + prompt.height - 1,
            prompt.width.saturating_sub(2),
            1,
        ),
    );
    let _ = input_style;
}

fn should_quit(event: Event) -> bool {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Esc, ..
        }) => true,
        Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers,
            ..
        }) => modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn editor() -> PromptEditor {
        PromptEditor::default()
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn inserts_and_deletes_unicode_by_character() {
        let mut e = editor();
        e.handle_event(key(KeyCode::Char('é')));
        e.handle_event(key(KeyCode::Char('x')));
        assert_eq!(e.lines, vec!["éx"]);
        e.handle_event(key(KeyCode::Backspace));
        assert_eq!(e.lines, vec!["é"]);
        e.handle_event(key(KeyCode::Backspace));
        assert_eq!(e.lines, vec![""]);
    }

    #[test]
    fn shift_enter_newlines_but_plain_enter_does_not() {
        let mut e = editor();
        assert!(!e.handle_event(key(KeyCode::Enter)));
        assert!(e.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT
        ))));
        assert_eq!(e.lines.len(), 2);
    }

    #[test]
    fn q_is_text_and_exit_keys_are_reserved() {
        let mut e = editor();
        assert!(e.handle_event(key(KeyCode::Char('q'))));
        assert_eq!(e.lines[0], "q");
        assert!(should_quit(key(KeyCode::Esc)));
        assert!(should_quit(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))));
    }

    #[test]
    fn visual_rows_wrap_and_keep_explicit_lines() {
        let mut e = editor();
        e.lines = vec!["abcd".into(), "é".into()];
        let rows = e.visual_rows(3);
        assert_eq!(
            rows.iter().map(|r| r.3.as_str()).collect::<Vec<_>>(),
            vec!["abc", "d", "é"]
        );
    }

    #[test]
    fn navigation_and_delete_join_lines() {
        let mut e = editor();
        e.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT,
        )));
        e.handle_event(key(KeyCode::Char('a')));
        e.handle_event(key(KeyCode::Up));
        e.handle_event(key(KeyCode::Delete));
        assert_eq!(e.lines, vec!["a"]);
        e.handle_event(key(KeyCode::Home));
        e.handle_event(key(KeyCode::Right));
        assert_eq!(e.cursor_col, 1);
    }

    #[test]
    fn cursor_scroll_is_bounded_to_visible_rows() {
        let mut e = editor();
        e.lines = (0..10).map(|n| n.to_string()).collect();
        let rows = e.visual_rows(10);
        let cursor = rows.len() - 1;
        let viewport = 3;
        e.vertical_scroll = (cursor + 1 - viewport).min(rows.len() - viewport);
        assert_eq!(e.vertical_scroll, 7);
    }

    #[test]
    fn non_empty_enter_starts_chat_and_preserves_multiline_message() {
        let mut app = App::default();
        app.handle_event(key(KeyCode::Char('a')));
        app.handle_event(modified_key(KeyCode::Enter, KeyModifiers::SHIFT));
        app.handle_event(key(KeyCode::Char('b')));
        assert!(app.handle_event(key(KeyCode::Enter)));
        assert_eq!(app.layout, LayoutMode::Chat);
        assert_eq!(app.editor.text(), "");
        assert!(matches!(app.timeline.first(), Some(TimelineEntry::User(text)) if text == "a\nb"));
        assert!(
            matches!(app.timeline.get(1), Some(TimelineEntry::Workflow(workflow))
            if workflow.workflow.nodes[0].status == NodeStatus::Running)
        );
        assert!(app.pending.is_some());
    }

    #[test]
    fn whitespace_enter_is_ignored_and_shift_enter_still_inserts_newline() {
        let mut app = App::default();
        app.handle_event(key(KeyCode::Char(' ')));
        assert!(!app.handle_event(key(KeyCode::Enter)));
        assert_eq!(app.layout, LayoutMode::Landing);
        app.handle_event(modified_key(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.editor.lines, vec![" ".to_string(), String::new()]);
    }

    #[test]
    fn spinner_only_advances_while_pending() {
        let mut app = App::default();
        assert!(!app.tick());
        app.editor.insert('x');
        app.submit();
        assert!(app.tick());
        assert_eq!(app.pending.as_ref().unwrap().animation_frame, 1);
        for _ in 0..workflow_spec().nodes.len() {
            app.advance_demo();
        }
        assert!(!app.tick());
    }

    #[test]
    fn ctrl_one_completes_demo_and_preserves_pending_draft() {
        let mut app = App::default();
        app.editor.insert('x');
        app.submit();
        app.editor.insert('d');
        assert!(!app.handle_event(key(KeyCode::Enter)));
        assert_eq!(app.editor.text(), "d");
        assert!(app.handle_event(modified_key(KeyCode::Char('1'), KeyModifiers::CONTROL,)));
        assert!(app.pending.is_some());
        assert_eq!(app.editor.text(), "d");
        for _ in 1..workflow_spec().nodes.len() {
            app.advance_demo();
        }
        assert!(app.pending.is_none());
        assert!(
            matches!(app.timeline.last(), Some(TimelineEntry::Assistant(text)) if text.starts_with("Implemented"))
        );
        assert!(!app.handle_event(modified_key(KeyCode::Char('1'), KeyModifiers::CONTROL,)));
    }

    #[test]
    fn completed_turn_can_start_another_demo() {
        let mut app = App::default();
        app.editor.insert('x');
        app.submit();
        for _ in 0..workflow_spec().nodes.len() {
            app.advance_demo();
        }
        app.editor.insert('y');
        assert!(app.handle_event(key(KeyCode::Enter)));
        assert!(app.pending.is_some());
        assert_eq!(app.timeline.len(), 5);
        assert_eq!(app.timeline[3], TimelineEntry::User("y".into()));
    }

    #[test]
    fn workflow_reveals_nodes_and_recovers_from_format_failure() {
        let mut app = App::default();
        app.editor.insert('x');
        app.submit();
        assert_eq!(
            app.workflow
                .as_ref()
                .unwrap()
                .workflow
                .nodes
                .iter()
                .filter(|n| n.status != NodeStatus::Pending)
                .count(),
            1
        );
        for step in 0..workflow_spec().nodes.len() {
            app.advance_demo();
            let workflow = app.workflow.as_ref().unwrap();
            if step == 9 {
                assert_eq!(workflow.workflow.nodes[9].status, NodeStatus::Failed);
                assert_eq!(
                    workflow.workflow.nodes[9].outcome.as_deref(),
                    Some("format check failed")
                );
            }
            if step < workflow_spec().nodes.len() - 1 {
                assert_eq!(
                    workflow
                        .workflow
                        .nodes
                        .iter()
                        .filter(|n| n.status == NodeStatus::Running)
                        .count(),
                    1
                );
            }
        }
        let workflow = app.workflow.as_ref().unwrap();
        assert_eq!(workflow.workflow.nodes.len(), 13);
        assert_eq!(workflow.workflow.nodes[9].status, NodeStatus::Failed);
        assert_eq!(
            workflow.workflow.nodes[11].outcome.as_deref(),
            Some("185 tests passed")
        );
        assert!(app.pending.is_none());
        assert!(
            matches!(app.timeline.last(), Some(TimelineEntry::Assistant(text)) if text.contains("185 tests passed"))
        );
    }

    #[test]
    fn timeline_uses_full_width_and_workflow_merge_connector() {
        let mut workflow = workflow_spec();
        for node in &mut workflow.nodes {
            node.status = NodeStatus::Done;
        }
        let workflow = WorkflowInstance { workflow };
        let lines = timeline_lines(&[TimelineEntry::Workflow(workflow)], 100, None);
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("2 inputs"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("185 tests passed"))
        );
        assert!(
            timeline_lines(&[TimelineEntry::Assistant("x".into())], 100, None).len()
                < timeline_lines(&[TimelineEntry::Assistant("x".into())], 20, None).len() + 2
        );
    }

    #[test]
    fn ansi_logo_has_three_rows_and_stable_spacing() {
        assert_eq!(ANSI_COMPACT.len(), 3);
        let widths: Vec<_> = ANSI_COMPACT
            .iter()
            .map(|(hay, cut)| hay.chars().count() + cut.chars().count())
            .collect();
        let canvas_width = *widths.iter().max().unwrap();
        assert!(widths.iter().all(|width| *width <= canvas_width));
        assert!(widths[0] > TAGLINE.chars().count());
        let lines = ansi_logo_lines();
        assert_eq!(lines.len(), LOGO_CANVAS_HEIGHT as usize);
        assert!(lines.iter().all(|line| line.spans.len() == 2));
        assert!(lines.iter().all(|line| {
            line.spans[0].style.fg == Some(Color::Yellow)
                && line.spans[1].style.fg == Some(Color::Green)
        }));
    }

    #[test]
    fn landing_variants_switch_at_size_boundaries() {
        let full_width = ANSI_COMPACT
            .iter()
            .map(|(hay, cut)| hay.chars().count() + cut.chars().count())
            .max()
            .unwrap_or(0) as u16;
        assert_eq!(
            landing_variant(Rect::new(0, 0, full_width, 5)),
            LandingVariant::Full
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, full_width.saturating_sub(1), 5)),
            LandingVariant::Compact
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, "HayCut".chars().count() as u16, 2)),
            LandingVariant::Compact
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, "HayCut".chars().count() as u16 - 1, 3)),
            LandingVariant::Hidden
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, "HayCut".chars().count() as u16, 1)),
            LandingVariant::Hidden
        );
    }

    #[test]
    fn metadata_is_versioned_and_fixed_width() {
        assert_eq!(metadata(false), "v0.1.0");
        assert!(metadata(true).contains(" - "));
        assert_eq!(BUILD_SHA.chars().count(), 8);
    }

    #[test]
    fn prompt_starts_at_three_rows_and_grows_to_half_screen() {
        let area = Rect::new(0, 0, 80, 30);
        let initial = prompt_rect_for(area, PROMPT_MIN_CONTENT_ROWS);
        assert_eq!(initial.width, 76);
        assert_eq!(initial.height.saturating_sub(2), 3);
        assert_eq!(initial.y + initial.height + BOTTOM_MARGIN, area.height);
        let grown = prompt_rect_for(area, 20);
        assert_eq!(grown.height, area.height / 2);
        assert_eq!(grown.y + grown.height + BOTTOM_MARGIN, area.height);
    }

    #[test]
    fn message_rails_are_mirrored_and_keep_body_unstyled() {
        let role_style = Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD);
        let assistant = assistant_block_lines("hello", 20, role_style);
        let user = user_block_lines("hello", 20, Style::default().fg(Color::Yellow));
        assert_eq!(assistant.len(), 3);
        assert_eq!(user.len(), 3);
        assert_eq!(assistant[0].spans[0].content, "╭ ");
        assert_eq!(assistant[1].spans[0].content, "│ ");
        assert_eq!(assistant[1].spans[1].style, Style::default());
        assert_eq!(user[0].spans.last().unwrap().content, "You ╮");
        assert_eq!(user[2].spans.last().unwrap().content, "╯");
    }

    #[test]
    fn landing_is_centered_in_space_above_prompt_for_odd_resize() {
        let area = Rect::new(0, 0, 81, 21);
        let prompt = prompt_rect_for(area, PROMPT_MIN_CONTENT_ROWS);
        let landing = Rect::new(area.x, area.y, area.width, prompt.y - area.y);
        let content = Rect::new(landing.x, landing.y + 1, landing.width, landing.height - 1);
        let height = ANSI_COMPACT.len() as u16 + 2;
        let branding_y = content.y + content.height.saturating_sub(height) / 2;
        assert!(branding_y + height <= prompt.y);
        assert_eq!(landing.width, area.width);
    }

    #[test]
    fn event_history_reduces_to_the_same_semantic_workflow_document() {
        let events = vec![
            UiEvent::UserSubmitted {
                id: 1,
                text: "build it".into(),
            },
            UiEvent::WorkflowStarted { id: 2 },
            UiEvent::NodeStarted {
                workflow_id: 2,
                node_id: 0,
            },
            UiEvent::NodeCompleted {
                workflow_id: 2,
                node_id: 0,
                outcome: Some("implement feature"),
            },
            UiEvent::NodeStarted {
                workflow_id: 2,
                node_id: 1,
            },
            UiEvent::NodeFailed {
                workflow_id: 2,
                node_id: 1,
                outcome: "format check failed",
            },
            UiEvent::AssistantResponse {
                id: 3,
                text: "done".into(),
            },
        ];
        let document = reduce_events(&events);
        assert!(matches!(&document[0], TimelineEntry::User(text) if text == "build it"));
        let TimelineEntry::Workflow(workflow) = &document[1] else {
            panic!("workflow missing")
        };
        assert_eq!(workflow.workflow.nodes[0].status, NodeStatus::Done);
        assert_eq!(workflow.workflow.nodes[1].status, NodeStatus::Failed);
        assert!(matches!(&document[2], TimelineEntry::Assistant(text) if text == "done"));
    }

    #[test]
    fn viewport_navigation_clamps_and_restores_follow_tail() {
        let mut viewport = ChatViewport {
            height: 5,
            follow_tail: true,
            ..ChatViewport::default()
        };
        viewport.set_offset(25, 30);
        assert_eq!(viewport.offset, 25);
        assert!(viewport.follow_tail);
        viewport.scroll_by(-1, 30);
        assert_eq!(viewport.offset, 24);
        assert!(!viewport.follow_tail);
        viewport.scroll_by(100, 30);
        assert_eq!(viewport.offset, 25);
        assert!(viewport.follow_tail);
        assert_eq!(viewport.unseen_events, 0);
    }

    #[test]
    fn rendered_rows_keep_semantic_metadata_when_wrapping() {
        let entries = vec![TimelineEntry::User("a long message".into())];
        let rows = rendered_rows(&entries, 8, None);
        assert!(rows.len() > 3);
        assert!(rows.iter().all(|row| row.owner == "user-0"));
        assert_eq!(rows[0].local_row, 0);
        assert_eq!(rows[0].key, "user-0-0");
    }

    #[test]
    fn hit_testing_returns_semantic_targets() {
        let regions = vec![HitRegion {
            rect: Rect::new(4, 5, 3, 1),
            target: HitTarget::ToggleDetails {
                item_id: "node-1".into(),
            },
        }];
        assert_eq!(
            hit_test(&regions, 5, 5),
            Some(HitTarget::ToggleDetails {
                item_id: "node-1".into()
            })
        );
        assert_eq!(hit_test(&regions, 0, 0), None);
    }

    #[test]
    fn tabs_switch_without_touching_prompt_or_demo_progress() {
        let mut app = App::default();
        assert_eq!(app.active_tab, ActiveTab::Chat);
        app.editor.insert('x');
        app.submit();
        assert_eq!(app.active_tab, ActiveTab::Chat);
        assert!(app.handle_event(modified_key(KeyCode::Right, KeyModifiers::CONTROL)));
        assert_eq!(app.active_tab, ActiveTab::Workflow);
        assert_eq!(app.editor.text(), "");
        for ch in "draft".chars() {
            app.editor.insert(ch);
        }
        assert!(app.handle_event(modified_key(KeyCode::Left, KeyModifiers::CONTROL)));
        assert_eq!(app.active_tab, ActiveTab::Chat);
        assert_eq!(app.editor.text(), "draft");
        assert_eq!(
            app.workflow.as_ref().unwrap().workflow.nodes[0].status,
            NodeStatus::Running
        );
    }

    #[test]
    fn projection_exposes_real_executor_and_dependency_data() {
        let workflow = workflow_spec();
        let projection = workflow_projection(&workflow);
        assert_eq!(projection.nodes[0].label, "Classify Intent");
        assert_eq!(projection.nodes[0].executor, ExecutorKind::WeakModel);
        assert_eq!(projection.nodes[7].dependencies, vec!["source", "tests"]);
        assert_eq!(projection.nodes[7].status, NodeStatus::Pending);
    }

    #[test]
    fn context_support_is_responsive_and_chat_first() {
        assert!(context_supported(96, ActiveTab::Chat));
        assert!(context_supported(96, ActiveTab::Workflow));
        assert!(context_supported(80, ActiveTab::Chat));
        assert!(!context_supported(80, ActiveTab::Workflow));
        assert!(!context_supported(69, ActiveTab::Chat));
    }

    #[test]
    fn context_sidebar_has_collapsible_sections_and_budget_state() {
        let snapshot = ContextSidebarSnapshot {
            packet_tokens: 41_000,
            packet_hard_limit: 40_000,
            ..ContextSidebarSnapshot::default()
        };
        let (rows, hits) = context_rows(&snapshot, 36, &HashSet::new());
        assert!(rows.iter().any(|row| row.key == "Tokens-0"));
        assert!(hits.iter().any(|hit| matches!(&hit.target, HitTarget::ToggleDetails { item_id } if item_id == "section:Tokens")));
        let (collapsed, _) = context_rows(&snapshot, 36, &HashSet::from(["Tokens".to_string()]));
        assert!(collapsed.len() < rows.len());
        assert_eq!(budget_style(&snapshot).fg, Some(Color::Red));
    }

    #[test]
    fn ctrl_b_is_consumed_and_preserves_prompt_draft() {
        let mut app = App::default();
        app.editor.insert('x');
        app.submit();
        for ch in "draft".chars() {
            app.editor.insert(ch);
        }
        assert!(app.handle_event(modified_key(KeyCode::Char('b'), KeyModifiers::CONTROL)));
        assert!(app.context_open);
        assert_eq!(app.editor.text(), "draft");
        assert!(app.handle_event(modified_key(KeyCode::Char('b'), KeyModifiers::CONTROL)));
        assert!(!app.context_open);
    }

    #[test]
    fn usage_format_preserves_provenance_and_lower_bounds() {
        assert_eq!(format_usage(UsageValue::Estimated(1_800)), "≈1.8k");
        assert_eq!(format_usage(UsageValue::Reported(412)), "412");
        assert_eq!(format_usage(UsageValue::LowerBound(6_200)), "≥6.2k");
        assert_eq!(format_usage(UsageValue::Unknown), "—");
        assert_eq!(
            add_usage(UsageValue::Reported(100), UsageValue::Unknown),
            UsageValue::LowerBound(100)
        );
    }

    #[test]
    fn active_usage_is_not_double_counted_in_session_totals() {
        let events = vec![
            UiEvent::ModelStream(ModelStreamEvent::CallStarted {
                id: "a".into(),
                purpose: "planning patch".into(),
                input: UsageValue::Estimated(1_800),
            }),
            UiEvent::ModelStream(ModelStreamEvent::UsageUpdate {
                id: "a".into(),
                input: UsageValue::Estimated(1_800),
                output: UsageValue::Estimated(64),
                cached_input: UsageValue::Unknown,
            }),
        ];
        let meter = model_meter(&events);
        assert_eq!(meter.input_total, UsageValue::Unknown);
        assert_eq!(
            meter.active.as_ref().unwrap().output,
            UsageValue::Estimated(64)
        );
        let mut completed = events;
        completed.push(UiEvent::ModelStream(ModelStreamEvent::CallCompleted {
            id: "a".into(),
            input: UsageValue::Reported(1_800),
            output: UsageValue::Reported(72),
            cached_input: UsageValue::Unknown,
        }));
        let meter = model_meter(&completed);
        assert_eq!(meter.active, None);
        assert_eq!(meter.input_total, UsageValue::LowerBound(1_800));
        assert_eq!(meter.output_total, UsageValue::LowerBound(72));
    }

    #[test]
    fn demo_model_node_ticks_emit_monotonic_output_usage() {
        let mut app = App::default();
        app.editor.insert('x');
        app.submit();
        let before = model_meter(&app.events).active.unwrap().output;
        app.tick();
        let after = model_meter(&app.events).active.unwrap().output;
        assert!(
            matches!((before, after), (UsageValue::Unknown, UsageValue::Estimated(value)) if value > 0)
        );
    }
}
