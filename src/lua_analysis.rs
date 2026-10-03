//! Understanding a script's Lua, for the editor: parsing it (full_moon,
//! which copes with half-typed code), working out every local's scope and
//! what each name refers to, and from that: warnings (unknown or misspelled
//! names, unused locals, globals set by accident), completions at the
//! cursor, signature help inside a call, and an outline of its functions.
//!
//! Positions are byte offsets into the analyzed source unless said
//! otherwise; [`Analysis::char_index`] converts for the editor.

use std::collections::HashSet;

use full_moon::ast::{self, Expression, Parameter, Prefix, Var};
use full_moon::node::Node;
use full_moon::tokenizer::TokenReference;
use full_moon::visitors::Visitor;
use full_moon::LuaVersion;

use crate::lua_completion::HOST_API;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    Variable,
    Parameter,
    Function,
    LoopVariable,
}

/// A table whose fields we know: what `playback()` returns, say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Fields(&'static [&'static str]),
    /// A list of tables with these fields.
    ListOf(&'static [&'static str]),
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub kind: LocalKind,
    /// Where its name is written.
    pub at: usize,
    pub line: usize,
    /// It can be used from here...
    pub visible_from: usize,
    /// ...up to here (the end of its block).
    pub scope_end: usize,
    pub shape: Option<Shape>,
    /// For a local function: its parameters.
    pub params: Option<Vec<String>>,
    pub used: bool,
}

/// A name used in the code, and the local it means (`None`: a global).
#[derive(Clone, Debug)]
pub struct Ref {
    pub name: String,
    pub at: usize,
    pub end: usize,
    pub local: Option<usize>,
}

/// A function the script defines, for the outline and signature help.
#[derive(Clone, Debug)]
pub struct Function {
    /// As written: `render`, `M.update`, `obj:draw`.
    pub name: String,
    pub params: Vec<String>,
    pub at: usize,
    pub line: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub message: String,
    pub severity: Severity,
}

#[derive(Default)]
pub struct Analysis {
    pub source: String,
    pub locals: Vec<Local>,
    pub refs: Vec<Ref>,
    pub functions: Vec<Function>,
    pub diagnostics: Vec<Diagnostic>,
    /// Globals the script itself defines (assigns, or declares functions).
    pub script_globals: HashSet<String>,
    /// The byte offset of each char, for converting to char indices.
    char_starts: Vec<usize>,
}

impl Analysis {
    /// The char index of byte offset `byte` in the analyzed source.
    pub fn char_index(&self, byte: usize) -> usize {
        self.char_starts.partition_point(|&b| b < byte)
    }

    /// The byte offset of char index `index`.
    pub fn byte_index(&self, index: usize) -> usize {
        self.char_starts.get(index).copied().unwrap_or(self.source.len())
    }

    /// The name used at byte `at` (or ending there), if any.
    pub fn ref_at(&self, at: usize) -> Option<&Ref> {
        self.refs.iter().find(|r| r.at <= at && at <= r.end)
    }

    /// Locals visible at byte `at`, innermost last.
    pub fn locals_at(&self, at: usize) -> impl Iterator<Item = &Local> {
        self.locals.iter().filter(move |l| l.visible_from <= at && at < l.scope_end)
    }

    /// The local named `name` visible at `at`.
    pub fn local_named(&self, name: &str, at: usize) -> Option<&Local> {
        self.locals_at(at).filter(|l| l.name == name).max_by_key(|l| l.visible_from)
    }
}

/// What a call to a host function returns, when it's a table we know.
pub fn shape_of_call(function: &str) -> Option<Shape> {
    const PLAYBACK: &[&str] = &[
        "position", "length", "speed", "paused", "finished", "loop_enabled", "generation", "song_name", "song_path",
        "song_id",
    ];
    const TYPING: &[&str] = &["active", "done", "cancelled", "text", "cursor", "line", "column", "key"];
    const ACTIVE: &[&str] = &["channel", "key", "velocity", "source"];
    const UPCOMING: &[&str] = &["channel", "key", "velocity", "on", "seconds_until"];
    const WHOLE: &[&str] = &["id", "channel", "key", "velocity", "start", "stop"];
    match function {
        "playback" => Some(Shape::Fields(PLAYBACK)),
        "typing_state" => Some(Shape::Fields(TYPING)),
        "active_notes" => Some(Shape::ListOf(ACTIVE)),
        "upcoming_notes" => Some(Shape::ListOf(UPCOMING)),
        "notes_between" => Some(Shape::ListOf(WHOLE)),
        _ => None,
    }
}

/// Lua's own globals scripts have (the base library, `math`, `string`,
/// `table`), with signatures.
pub const LUA_GLOBALS: &[(&str, &str)] = &[
    ("assert", "assert(v, [message]) -> v: error with `message` unless v is truthy"),
    ("error", "error(message, [level]): raise an error"),
    ("ipairs", "ipairs(t): iterate 1, t[1]; 2, t[2]; ... up to the first nil"),
    ("next", "next(t, [key]) -> key, value: the next entry after `key`"),
    ("pairs", "pairs(t): iterate every key, value of t (in no particular order)"),
    ("pcall", "pcall(f, ...) -> ok, result...: call f, catching errors"),
    ("print", "print(...): no console here; use log()"),
    ("rawequal", "rawequal(a, b) -> bool"),
    ("rawget", "rawget(t, key) -> value"),
    ("rawlen", "rawlen(t) -> length"),
    ("rawset", "rawset(t, key, value)"),
    ("select", "select(n, ...): the arguments from the n-th; select('#', ...) counts them"),
    ("setmetatable", "setmetatable(t, mt) -> t"),
    ("getmetatable", "getmetatable(t) -> mt"),
    ("tonumber", "tonumber(v, [base]) -> number or nil"),
    ("tostring", "tostring(v) -> string"),
    ("type", "type(v) -> \"nil\", \"number\", \"string\", \"boolean\", \"table\" or \"function\""),
    ("xpcall", "xpcall(f, handler, ...) -> ok, result..."),
    ("math", "the math library: math.floor, math.sin, math.random, math.pi, ..."),
    ("string", "the string library: string.format, string.sub, ..."),
    ("table", "the table library: table.insert, table.remove, table.sort, ..."),
];

pub const MATH: &[(&str, &str)] = &[
    ("abs", "math.abs(x) -> |x|"),
    ("ceil", "math.ceil(x) -> the smallest integer >= x"),
    ("floor", "math.floor(x) -> the largest integer <= x"),
    ("sqrt", "math.sqrt(x) -> square root"),
    ("sin", "math.sin(x) -> sine of x radians"),
    ("cos", "math.cos(x) -> cosine of x radians"),
    ("tan", "math.tan(x) -> tangent of x radians"),
    ("asin", "math.asin(x) -> arc sine, radians"),
    ("acos", "math.acos(x) -> arc cosine, radians"),
    ("atan", "math.atan(y, [x]) -> arc tangent of y/x, radians, using both signs"),
    ("exp", "math.exp(x) -> e^x"),
    ("log", "math.log(x, [base]) -> logarithm (natural by default)"),
    ("max", "math.max(x, ...) -> the largest"),
    ("min", "math.min(x, ...) -> the smallest"),
    ("fmod", "math.fmod(x, y) -> remainder of x / y, rounded toward zero"),
    ("modf", "math.modf(x) -> integer part, fractional part"),
    ("random", "math.random([m, [n]]) -> a float in [0, 1), or an integer in [1, m] / [m, n]"),
    ("randomseed", "math.randomseed([x]): seed the generator"),
    ("tointeger", "math.tointeger(x) -> integer, or nil if x isn't a whole number"),
    ("type", "math.type(x) -> \"integer\", \"float\" or nil"),
    ("ult", "math.ult(m, n) -> m < n as unsigned integers"),
    ("pi", "math.pi: 3.14159..."),
    ("huge", "math.huge: infinity"),
    ("maxinteger", "math.maxinteger"),
    ("mininteger", "math.mininteger"),
];

pub const STRING: &[(&str, &str)] = &[
    ("byte", "string.byte(s, [i, [j]]) -> the character codes"),
    ("char", "string.char(...) -> a string from character codes"),
    ("find", "string.find(s, pattern, [init, [plain]]) -> start, end, captures..."),
    ("format", "string.format(format, ...) -> e.g. string.format(\"%.2f\", x)"),
    ("gmatch", "string.gmatch(s, pattern): iterate over the matches"),
    ("gsub", "string.gsub(s, pattern, replacement, [n]) -> new string, count"),
    ("len", "string.len(s) -> length in bytes (same as #s)"),
    ("lower", "string.lower(s) -> lowercase"),
    ("match", "string.match(s, pattern, [init]) -> captures (or the whole match)"),
    ("rep", "string.rep(s, n, [separator]) -> s repeated n times"),
    ("reverse", "string.reverse(s) -> s backwards"),
    ("sub", "string.sub(s, i, [j]) -> the part from i to j (negative counts from the end)"),
    ("upper", "string.upper(s) -> uppercase"),
];

pub const TABLE: &[(&str, &str)] = &[
    ("concat", "table.concat(list, [separator, [i, [j]]]) -> the items joined into a string"),
    ("insert", "table.insert(list, [position,] value): add value (at the end by default)"),
    ("move", "table.move(a1, from, to, at, [a2]) -> a2: copy a1[from..to] to a2[at...]"),
    ("pack", "table.pack(...) -> {...} with n set"),
    ("remove", "table.remove(list, [position]) -> the removed value (the last by default)"),
    ("sort", "table.sort(list, [comes_before]): sort in place"),
    ("unpack", "table.unpack(list, [i, [j]]) -> list[i], ..., list[j]"),
];

pub const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in", "local", "nil",
    "not", "or", "repeat", "return", "then", "true", "until", "while",
];

fn library(name: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match name {
        "math" => Some(MATH),
        "string" => Some(STRING),
        "table" => Some(TABLE),
        _ => None,
    }
}

fn token_name(token: &TokenReference) -> String {
    token.token().to_string()
}

fn start(token: &TokenReference) -> usize {
    token.token().start_position().bytes()
}

fn end(token: &TokenReference) -> usize {
    token.token().end_position().bytes()
}

fn line(token: &TokenReference) -> usize {
    token.token().start_position().line()
}

fn param_names(body: &ast::FunctionBody) -> Vec<String> {
    body.parameters()
        .iter()
        .map(|p| match p {
            Parameter::Name(name) => token_name(name),
            Parameter::Ellipsis(_) => "...".to_string(),
            _ => "?".to_string(),
        })
        .collect()
}

/// `f(...)` with `f` a plain name: that name.
fn called_name(expression: &Expression) -> Option<String> {
    let Expression::FunctionCall(call) = expression else { return None };
    let Prefix::Name(name) = call.prefix() else { return None };
    let mut suffixes = call.suffixes();
    match (suffixes.next(), suffixes.next()) {
        (Some(ast::Suffix::Call(_)), None) => Some(token_name(name)),
        _ => None,
    }
}

/// The element shape of `ipairs(f())` / `pairs(f())` when `f` returns a
/// list we know.
fn iterated_shape(expression: &Expression) -> Option<Shape> {
    let Expression::FunctionCall(call) = expression else { return None };
    let Prefix::Name(name) = call.prefix() else { return None };
    if !matches!(token_name(name).as_str(), "ipairs" | "pairs") {
        return None;
    }
    let Some(ast::Suffix::Call(ast::Call::AnonymousCall(ast::FunctionArgs::Parentheses { arguments, .. }))) =
        call.suffixes().next()
    else {
        return None;
    };
    let inner = arguments.iter().next()?;
    match shape_of_call(&called_name(inner)?) {
        Some(Shape::ListOf(fields)) => Some(Shape::Fields(fields)),
        _ => None,
    }
}

#[derive(Default)]
struct Collector {
    scope_ends: Vec<usize>,
    function_depth: usize,
    locals: Vec<Local>,
    reads: Vec<(String, usize, usize)>,
    /// Plain-name assignment targets: (name, at, end, inside a function).
    writes: Vec<(String, usize, usize, bool)>,
    write_positions: HashSet<usize>,
    functions: Vec<Function>,
    global_functions: HashSet<String>,
    /// A method's body is next: it gets an implicit `self`.
    pending_self: bool,
}

impl Collector {
    fn scope_end(&self) -> usize {
        self.scope_ends.last().copied().unwrap_or(usize::MAX)
    }

    fn declare(&mut self, token: &TokenReference, kind: LocalKind, visible_from: usize) -> usize {
        self.locals.push(Local {
            name: token_name(token),
            kind,
            at: start(token),
            line: line(token),
            visible_from,
            scope_end: self.scope_end(),
            shape: None,
            params: None,
            used: false,
        });
        self.locals.len() - 1
    }
}

impl Visitor for Collector {
    fn visit_function_body(&mut self, body: &ast::FunctionBody) {
        self.scope_ends.push(start(body.end_token()));
        self.function_depth += 1;
        let from = body.parameters_parentheses().tokens().1.token().end_position().bytes();
        if std::mem::take(&mut self.pending_self) {
            self.locals.push(Local {
                name: "self".to_string(),
                kind: LocalKind::Parameter,
                at: from,
                line: 0,
                visible_from: from,
                scope_end: self.scope_end(),
                shape: None,
                params: None,
                used: true,
            });
        }
        for parameter in body.parameters() {
            if let Parameter::Name(name) = parameter {
                self.declare(name, LocalKind::Parameter, from);
            }
        }
    }

    fn visit_function_body_end(&mut self, _: &ast::FunctionBody) {
        self.scope_ends.pop();
        self.function_depth -= 1;
    }

    fn visit_do(&mut self, node: &ast::Do) {
        self.scope_ends.push(start(node.end_token()));
    }
    fn visit_do_end(&mut self, _: &ast::Do) {
        self.scope_ends.pop();
    }
    fn visit_while(&mut self, node: &ast::While) {
        self.scope_ends.push(start(node.end_token()));
    }
    fn visit_while_end(&mut self, _: &ast::While) {
        self.scope_ends.pop();
    }
    fn visit_if(&mut self, node: &ast::If) {
        self.scope_ends.push(start(node.end_token()));
    }
    fn visit_if_end(&mut self, _: &ast::If) {
        self.scope_ends.pop();
    }
    fn visit_repeat(&mut self, node: &ast::Repeat) {
        // `until` can see the block's locals.
        let end = node.until().end_position().map_or(usize::MAX, |p| p.bytes());
        self.scope_ends.push(end);
    }
    fn visit_repeat_end(&mut self, _: &ast::Repeat) {
        self.scope_ends.pop();
    }

    fn visit_numeric_for(&mut self, node: &ast::NumericFor) {
        self.scope_ends.push(start(node.end_token()));
        let from = start(node.do_token());
        self.declare(node.index_variable(), LocalKind::LoopVariable, from);
    }
    fn visit_numeric_for_end(&mut self, _: &ast::NumericFor) {
        self.scope_ends.pop();
    }

    fn visit_generic_for(&mut self, node: &ast::GenericFor) {
        self.scope_ends.push(start(node.end_token()));
        let from = start(node.do_token());
        let element = node.expressions().iter().next().and_then(iterated_shape);
        for (n, name) in node.names().iter().enumerate() {
            let index = self.declare(name, LocalKind::LoopVariable, from);
            if n == 1 {
                self.locals[index].shape = element;
            }
        }
    }
    fn visit_generic_for_end(&mut self, _: &ast::GenericFor) {
        self.scope_ends.pop();
    }

    fn visit_local_assignment_end(&mut self, node: &ast::LocalAssignment) {
        let from = node.end_position().map_or(0, |p| p.bytes());
        let values: Vec<&Expression> = node.expressions().iter().collect();
        for (n, name) in node.names().iter().enumerate() {
            let index = self.declare(name, LocalKind::Variable, from);
            match values.get(n) {
                Some(Expression::Function(function)) => {
                    let params = param_names(function.body());
                    self.locals[index].kind = LocalKind::Function;
                    self.locals[index].params = Some(params.clone());
                    self.functions.push(Function { name: token_name(name), params, at: start(name), line: line(name) });
                }
                Some(value) => self.locals[index].shape = called_name(value).and_then(|f| shape_of_call(&f)),
                None => {}
            }
        }
    }

    fn visit_local_function(&mut self, node: &ast::LocalFunction) {
        let name = node.name();
        let index = self.declare(name, LocalKind::Function, end(name));
        let params = param_names(node.body());
        self.locals[index].params = Some(params.clone());
        self.functions.push(Function { name: token_name(name), params, at: start(name), line: line(name) });
    }

    fn visit_function_declaration(&mut self, node: &ast::FunctionDeclaration) {
        let declared = node.name();
        let names: Vec<String> = declared.names().iter().map(token_name).collect();
        let mut full = names.join(".");
        if let Some(method) = declared.method_name() {
            full = format!("{full}:{}", token_name(method));
            self.pending_self = true;
        }
        if names.len() == 1 && declared.method_name().is_none() {
            self.global_functions.insert(names[0].clone());
        }
        let first = declared.names().iter().next();
        let (at, line_no) = first.map_or((0, 0), |t| (start(t), line(t)));
        // `function name()` where name is a local in scope assigns that
        // local; record the first name as a use either way.
        if let Some(first) = first
            && names.len() > 1
        {
            self.reads.push((token_name(first), start(first), end(first)));
        }
        self.functions.push(Function { name: full, params: param_names(node.body()), at, line: line_no });
    }

    fn visit_assignment(&mut self, node: &ast::Assignment) {
        for var in node.variables() {
            if let Var::Name(name) = var {
                self.write_positions.insert(start(name));
                self.writes.push((token_name(name), start(name), end(name), self.function_depth > 0));
            }
        }
    }

    fn visit_var(&mut self, var: &Var) {
        if let Var::Name(name) = var
            && !self.write_positions.contains(&start(name))
        {
            self.reads.push((token_name(name), start(name), end(name)));
        }
    }

    fn visit_prefix(&mut self, prefix: &Prefix) {
        if let Prefix::Name(name) = prefix {
            self.reads.push((token_name(name), start(name), end(name)));
        }
    }
}

/// The edit distance between `a` and `b`, for "did you mean".
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, &ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current.push((previous[j] + cost).min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

fn line_of(source: &str, byte: usize) -> usize {
    source[..byte.min(source.len())].matches('\n').count() + 1
}

/// Analyze `source`. `known` is every global a script has without defining
/// it (the app's functions and Lua's), to tell a typo from a real name.
pub fn analyze(source: &str, known: &HashSet<String>) -> Analysis {
    let result = full_moon::parse_fallible(source, LuaVersion::lua54());
    let mut collector = Collector::default();
    collector.visit_ast(result.ast());

    let mut analysis = Analysis {
        source: source.to_string(),
        char_starts: source.char_indices().map(|(b, _)| b).collect(),
        ..Analysis::default()
    };
    for error in result.errors() {
        let (from, to) = error.range();
        let start = from.bytes().min(source.len());
        let end = to.bytes().clamp(start, source.len());
        analysis.diagnostics.push(Diagnostic {
            start,
            end: end.max(start + 1).min(source.len().max(start)),
            line: line_of(source, start),
            message: error.error_message().to_string(),
            severity: Severity::Error,
        });
    }
    let syntax_errors = !analysis.diagnostics.is_empty();

    let mut locals = collector.locals;
    let resolve = |locals: &[Local], name: &str, at: usize| {
        locals
            .iter()
            .enumerate()
            .filter(|(_, l)| l.name == name && l.visible_from <= at && at < l.scope_end)
            .max_by_key(|(_, l)| l.visible_from)
            .map(|(i, _)| i)
    };
    for (name, at, end) in collector.reads {
        let local = resolve(&locals, &name, at);
        if let Some(i) = local {
            locals[i].used = true;
        }
        analysis.refs.push(Ref { name, at, end, local });
    }
    let mut global_writes: Vec<(String, usize, usize, bool)> = Vec::new();
    for (name, at, end, in_function) in collector.writes {
        let local = resolve(&locals, &name, at);
        if local.is_none() {
            global_writes.push((name.clone(), at, end, in_function));
        }
        analysis.refs.push(Ref { name, at, end, local });
    }
    analysis.script_globals = collector.global_functions;
    analysis.script_globals.extend(global_writes.iter().map(|w| w.0.clone()));
    analysis.functions = collector.functions;

    if !syntax_errors {
        // Unknown names, with a guess at what was meant.
        let mut candidates: Vec<String> = known.iter().cloned().collect();
        candidates.extend(analysis.script_globals.iter().cloned());
        let mut reported = HashSet::new();
        for r in &analysis.refs {
            if r.local.is_some() || known.contains(&r.name) || analysis.script_globals.contains(&r.name) {
                continue;
            }
            if !reported.insert((r.name.clone(), r.at)) {
                continue;
            }
            let mut names_here: Vec<String> = analysis.locals_at_list(&locals, r.at);
            names_here.extend(candidates.iter().cloned());
            let guess = names_here
                .iter()
                .filter(|c| c.len() > 2 && **c != r.name)
                .map(|c| (distance(&r.name, c), c))
                .filter(|(d, _)| *d <= 2)
                .min();
            let message = match guess {
                Some((_, guess)) => format!("`{}` isn't defined anywhere. Did you mean `{guess}`?", r.name),
                None => format!("`{}` isn't defined anywhere (a typo, or a missing `local`?)", r.name),
            };
            analysis.diagnostics.push(Diagnostic {
                start: r.at,
                end: r.end,
                line: line_of(source, r.at),
                message,
                severity: Severity::Warning,
            });
        }
        // Unused locals.
        for local in &locals {
            if !local.used
                && matches!(local.kind, LocalKind::Variable | LocalKind::Function)
                && !local.name.starts_with('_')
            {
                analysis.diagnostics.push(Diagnostic {
                    start: local.at,
                    end: local.at + local.name.len(),
                    line: local.line,
                    message: format!("`{}` is never used", local.name),
                    severity: Severity::Warning,
                });
            }
        }
        // Globals set inside a function and nowhere else: usually a missing
        // `local`.
        let top_level: HashSet<&str> =
            global_writes.iter().filter(|w| !w.3).map(|w| w.0.as_str()).collect();
        let mut warned = HashSet::new();
        for (name, at, end, in_function) in &global_writes {
            if *in_function
                && !top_level.contains(name.as_str())
                && !known.contains(name)
                && warned.insert(name.clone())
            {
                analysis.diagnostics.push(Diagnostic {
                    start: *at,
                    end: *end,
                    line: line_of(source, *at),
                    message: format!("This sets a global `{name}`. Did you mean `local {name}`?"),
                    severity: Severity::Warning,
                });
            }
        }
    }
    analysis.diagnostics.sort_by_key(|d| d.start);
    analysis.locals = locals;
    analysis
}

impl Analysis {
    fn locals_at_list(&self, locals: &[Local], at: usize) -> Vec<String> {
        locals.iter().filter(|l| l.visible_from <= at && at < l.scope_end).map(|l| l.name.clone()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Local,
    Parameter,
    Function,
    Host,
    Lua,
    Field,
    Keyword,
}

impl ItemKind {
    /// A one-letter tag for the list.
    pub fn tag(self) -> &'static str {
        match self {
            ItemKind::Local => "v",
            ItemKind::Parameter => "p",
            ItemKind::Function => "f",
            ItemKind::Host => "a",
            ItemKind::Lua => "l",
            ItemKind::Field => ".",
            ItemKind::Keyword => "k",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub label: String,
    pub kind: ItemKind,
    pub detail: String,
}

/// Completions at a cursor: replace chars `from..cursor` with an item.
#[derive(Clone, Debug)]
pub struct Completions {
    pub from: usize,
    pub items: Vec<Item>,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// What's before a `.` or `:` at char `dot` in `chars`: a plain name
/// (`math`, `p`) or a call of one with no arguments (`playback()`).
fn receiver(chars: &[char], dot: usize) -> Option<(String, bool)> {
    let mut end = dot;
    let mut call = false;
    if end >= 2 && chars[end - 1] == ')' && chars[end - 2] == '(' {
        call = true;
        end -= 2;
    }
    let mut start = end;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    (start < end && !(start > 0 && matches!(chars[start - 1], '.' | ':')))
        .then(|| (chars[start..end].iter().collect(), call))
}

/// Rank `items` against what's typed: prefix matches first, then names
/// containing it, case-insensitively; drop the rest.
fn filter(items: Vec<Item>, typed: &str) -> Vec<Item> {
    let typed_lower = typed.to_lowercase();
    let mut seen = HashSet::new();
    let mut ranked: Vec<(u8, Item)> = items
        .into_iter()
        .filter(|item| seen.insert(item.label.clone()))
        .filter_map(|item| {
            let label = item.label.to_lowercase();
            let rank = if label.starts_with(&typed_lower) {
                0
            } else if typed.len() >= 2 && label.contains(&typed_lower) {
                1
            } else {
                return None;
            };
            Some((rank, item))
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.label.len().cmp(&b.1.label.len())).then(a.1.label.cmp(&b.1.label)));
    ranked.into_iter().map(|(_, item)| item).collect()
}

/// Completions for the text cursor at char `cursor` in `text` (the
/// editor's current text; `analysis` can be a keystroke behind). `forced`:
/// asked for (Ctrl+Space), so show everything even with nothing typed.
pub fn complete(analysis: &Analysis, text: &str, cursor: usize, forced: bool) -> Option<Completions> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    if crate::code_edit::in_comment_or_string_at(&chars, cursor) {
        return None;
    }
    let mut from = cursor;
    while from > 0 && is_ident(chars[from - 1]) {
        from -= 1;
    }
    if from < cursor && chars[from].is_ascii_digit() {
        return None; // a number
    }
    let typed: String = chars[from..cursor].iter().collect();
    let byte = analysis.byte_index(cursor.min(analysis.char_starts.len()));

    // Fields: `x.` / `x:`.
    if from > 0 && matches!(chars[from - 1], '.' | ':') {
        if from >= 2 && chars[from - 2] == '.' {
            return None; // `..`, concatenation
        }
        let (name, call) = receiver(&chars, from - 1)?;
        let mut items: Vec<Item> = Vec::new();
        if !call && let Some(lib) = library(&name) {
            items.extend(lib.iter().map(|&(label, detail)| Item {
                label: label.to_string(),
                kind: ItemKind::Lua,
                detail: detail.to_string(),
            }));
        }
        let shape = if call {
            shape_of_call(&name)
        } else {
            analysis.local_named(&name, byte).and_then(|l| l.shape)
        };
        if let Some(Shape::Fields(fields)) = shape {
            items.extend(fields.iter().map(|f| Item { label: f.to_string(), kind: ItemKind::Field, detail: String::new() }));
        }
        if items.is_empty() && !call {
            // A table of the script's own: fields used on it elsewhere.
            let pattern = format!("{name}.");
            let mut fields: Vec<String> = text
                .match_indices(&pattern)
                .filter(|(i, _)| *i == 0 || !text[..*i].ends_with(is_ident))
                .map(|(i, _)| text[i + pattern.len()..].chars().take_while(|&c| is_ident(c)).collect::<String>())
                .filter(|f| !f.is_empty() && *f != typed)
                .collect();
            fields.sort();
            fields.dedup();
            items.extend(fields.into_iter().map(|f| Item { label: f, kind: ItemKind::Field, detail: String::new() }));
        }
        let items = filter(items, &typed);
        return (!items.is_empty()).then_some(Completions { from, items });
    }

    if typed.is_empty() && !forced {
        return None;
    }
    let mut items: Vec<Item> = Vec::new();
    let mut visible: Vec<&Local> = analysis.locals_at(byte).collect();
    visible.sort_by_key(|l| std::cmp::Reverse(l.visible_from));
    for local in visible {
        let (kind, detail) = match (local.kind, &local.params) {
            (LocalKind::Parameter, _) => (ItemKind::Parameter, "parameter".to_string()),
            (_, Some(params)) => (ItemKind::Function, format!("{}({}), line {}", local.name, params.join(", "), local.line)),
            _ => (ItemKind::Local, format!("local, line {}", local.line)),
        };
        items.push(Item { label: local.name.clone(), kind, detail });
    }
    for function in &analysis.functions {
        if analysis.script_globals.contains(&function.name) {
            items.push(Item {
                label: function.name.clone(),
                kind: ItemKind::Function,
                detail: format!("{}({}), line {}", function.name, function.params.join(", "), function.line),
            });
        }
    }
    let mut globals: Vec<&String> = analysis.script_globals.iter().collect();
    globals.sort();
    items.extend(globals.into_iter().map(|g| Item { label: g.clone(), kind: ItemKind::Local, detail: "global".into() }));
    items.extend(HOST_API.iter().map(|&(label, detail)| Item {
        label: label.to_string(),
        kind: ItemKind::Host,
        detail: detail.to_string(),
    }));
    items.extend(LUA_GLOBALS.iter().map(|&(label, detail)| Item {
        label: label.to_string(),
        kind: ItemKind::Lua,
        detail: detail.to_string(),
    }));
    items.extend(KEYWORDS.iter().map(|k| Item { label: k.to_string(), kind: ItemKind::Keyword, detail: String::new() }));
    let items = filter(items, &typed);
    // Nothing to offer, or exactly what's typed already.
    if items.is_empty() || (items.len() == 1 && items[0].label == typed) {
        return None;
    }
    Some(Completions { from, items })
}

/// The call the cursor is inside, for signature help.
#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    /// `name(` up to the parameters, e.g. `sprite`.
    pub name: String,
    pub params: Vec<String>,
    /// Which parameter the cursor is on.
    pub active: usize,
    /// What follows the parameters in the description (`-> width, height:
    /// draw a sprite`).
    pub rest: String,
}

/// Split a description like `sprite(id, x, y, [scale]) -> w, h: ...` into
/// its parameters and the rest.
fn parse_signature(detail: &str) -> Option<(Vec<String>, String)> {
    let open = detail.find('(')?;
    let mut depth = 0;
    let mut close = None;
    for (i, c) in detail[open..].char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let inner = &detail[open + 1..close];
    let mut params = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for c in inner.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                params.push(current.trim().to_string());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        params.push(current.trim().to_string());
    }
    Some((params, detail[close + 1..].trim().to_string()))
}

/// Signature help for the cursor at char `cursor`: the innermost call it's
/// inside, and which argument.
pub fn signature_at(analysis: &Analysis, text: &str, cursor: usize) -> Option<Signature> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let mask = crate::code_edit::code_mask_of(&chars);
    let mut depth = 0i32;
    let mut commas = 0;
    let mut i = cursor;
    let open = loop {
        if i == 0 {
            return None;
        }
        i -= 1;
        if !mask[i] {
            continue;
        }
        match chars[i] {
            ')' | ']' | '}' => depth += 1,
            '[' | '{' if depth == 0 => return None,
            '(' if depth == 0 => break i,
            '(' | '[' | '{' => depth -= 1,
            ',' if depth == 0 => commas += 1,
            '\n' if depth == 0 && cursor - i > 400 => return None,
            _ => {}
        }
    };
    let mut start = open;
    while start > 0 && (is_ident(chars[start - 1]) || chars[start - 1] == '.') {
        start -= 1;
    }
    let name: String = chars[start..open].iter().collect();
    if name.is_empty() || KEYWORDS.contains(&name.as_str()) {
        return None;
    }
    let lookup = |detail: &str| parse_signature(detail);
    let (params, rest) = if let Some((library_name, member)) = name.split_once('.') {
        let detail = library(library_name)?.iter().find(|(n, _)| *n == member)?.1;
        lookup(detail)?
    } else if let Some(&(_, detail)) = HOST_API.iter().chain(LUA_GLOBALS).find(|(n, _)| *n == name) {
        lookup(detail)?
    } else {
        let byte = analysis.byte_index(cursor.min(analysis.char_starts.len()));
        let params = analysis
            .local_named(&name, byte)
            .and_then(|l| l.params.clone())
            .or_else(|| analysis.functions.iter().find(|f| f.name == name).map(|f| f.params.clone()))?;
        (params, String::new())
    };
    if params.is_empty() {
        return None;
    }
    let active = commas.min(params.len() - 1);
    Some(Signature { name, params, active, rest })
}

/// The global names a script has without defining them: the app's (from a
/// real script VM, deprecated ones included) and Lua's.
pub fn known_globals(host: impl IntoIterator<Item = String>) -> HashSet<String> {
    let mut known: HashSet<String> = host.into_iter().collect();
    known.extend(LUA_GLOBALS.iter().map(|(n, _)| n.to_string()));
    known.extend(["_G", "_VERSION", "render"].map(String::from));
    known
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> HashSet<String> {
        known_globals(HOST_API.iter().map(|(n, _)| n.to_string()))
    }

    fn messages(source: &str) -> Vec<String> {
        analyze(source, &known()).diagnostics.iter().map(|d| format!("{}: {}", d.line, d.message)).collect()
    }

    #[test]
    fn typos_unused_locals_and_stray_globals_are_reported() {
        let source = "local count = 0\nlocal spare = 1\nfunction render(w, h)\n    local p = playbak()\n    total = p\n    count = count + w\nend\n";
        assert_eq!(
            messages(source),
            [
                "2: `spare` is never used",
                "4: `playbak` isn't defined anywhere. Did you mean `playback`?",
                "5: This sets a global `total`. Did you mean `local total`?",
            ]
        );
    }

    #[test]
    fn scopes_resolve_like_lua() {
        let source = "local x = 1\nfunction f(a)\n    local x = x + a\n    for i = 1, x do log(i) end\n    return x\nend\nlog(x, f(2))\n";
        let analysis = analyze(source, &known());
        assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);
        // The inner `x` reads the outer one; `return x` reads the inner one.
        let x_refs: Vec<(usize, Option<usize>)> =
            analysis.refs.iter().filter(|r| r.name == "x").map(|r| (line_of(source, r.at), r.local)).collect();
        let outer = analysis.locals.iter().position(|l| l.name == "x" && l.line == 1);
        let inner = analysis.locals.iter().position(|l| l.name == "x" && l.line == 3);
        assert_eq!(x_refs, [(3, outer), (4, inner), (5, inner), (7, outer)]);
        assert_eq!(analysis.functions.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["f"]);
    }

    #[test]
    fn half_typed_code_still_analyzes() {
        let analysis = analyze("local a = 1\nif a then\n    log(a\n", &known());
        assert!(analysis.diagnostics.iter().any(|d| d.severity == Severity::Error));
        assert!(analysis.locals.iter().any(|l| l.name == "a"));
    }

    fn completions(marked: &str, forced: bool) -> Vec<String> {
        let cursor = marked.chars().position(|c| c == '|').unwrap();
        let text: String = marked.chars().filter(|&c| c != '|').collect();
        let analysis = analyze(&text, &known());
        complete(&analysis, &text, cursor, forced).map(|c| c.items.into_iter().map(|i| i.label).collect()).unwrap_or_default()
    }

    #[test]
    fn completes_names_libraries_and_known_fields() {
        let first = completions("local speed_x = 1\nfunction render()\n    log(spe|)\nend", false);
        assert_eq!(first.first().map(String::as_str), Some("speed_x"));
        assert!(completions("math.fl|", false).contains(&"floor".to_string()));
        let fields = completions("local p = playback()\nlog(p.|)", false);
        assert!(fields.contains(&"song_id".to_string()) && fields.contains(&"position".to_string()), "{fields:?}");
        assert!(completions("log(playback().pos|)", false).contains(&"position".to_string()));
        let notes = completions("for _, n in ipairs(active_notes()) do log(n.|) end", false);
        assert!(notes.contains(&"velocity".to_string()), "{notes:?}");
        let own = completions("local sn = {}\nsn.body = {}\nsn.length = 3\nlog(sn.|)", false);
        assert_eq!(own, ["body", "length"]);
        assert!(completions("-- spe|", false).is_empty());
        assert!(completions("x = 1|", false).is_empty());
    }

    #[test]
    fn signature_help_finds_the_call_and_argument() {
        let help = |marked: &str| {
            let cursor = marked.chars().position(|c| c == '|').unwrap();
            let text: String = marked.chars().filter(|&c| c != '|').collect();
            signature_at(&analyze(&text, &known()), &text, cursor).map(|s| (s.name, s.params[s.active].clone()))
        };
        assert_eq!(help("rect(0, 0, |"), Some(("rect".into(), "x1".into())));
        assert_eq!(help("math.max(1, f(2, 3), |"), Some(("math.max".into(), "...".into())));
        assert_eq!(help("local function go(a, b) end\ngo(1, \"(x\", |)"), Some(("go".into(), "b".into())));
        assert_eq!(help("x = {1, |"), None);
    }
}
