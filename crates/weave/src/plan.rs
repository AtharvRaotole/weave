//! Static plan of a script: every tool call site, how many times it can run,
//! where its arguments come from, what it may write, and which earlier calls
//! it waits for. Built from the same lowered program the scheduler runs,
//! before any tool is called.
//!
//! The plan is an over-approximation of what the run can do, assuming no
//! tool fails: every call the run makes belongs to a call site listed here,
//! the number of calls lies within `[min_calls, max_calls]`, and a call's
//! arguments only carry data from the call sites listed in its sources.
//! Values the plan cannot see statically are reported as unknown, never
//! guessed.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::analysis::is_builtin;
use crate::ast::{Expr, ExprKind, FSeg, Stmt, StmtKind, Target, UnaryOp};
use crate::effect::Effect;
use crate::tools::{Footprint, ToolRegistry};
use crate::value::{py_repr, Value};

/// Values listed per argument, and resolved footprint names per call site,
/// are capped so a plan stays small.
const MAX_VALUES: usize = 16;
const MAX_NAMES: usize = 64;

/// Where one argument of a call site comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArgSource {
    /// Known before the run: one of these values (Python reprs).
    Literal { values: Vec<String> },
    /// Computed from the results of these call sites (and maybe literals).
    Tools { calls: Vec<usize> },
    /// Computed without any tool result, but not known statically.
    Computed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlannedArg {
    /// Parameter name: the keyword, or the declared name of a positional
    /// parameter; `None` for a positional argument of an undeclared tool.
    pub name: Option<String>,
    pub position: Option<usize>,
    pub source: ArgSource,
}

/// Resources a call site may touch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlannedFootprint {
    /// Undeclared: anything.
    All,
    /// Every name the call can touch, with literal arguments filled in.
    Names { names: Vec<String> },
    /// Declared templates whose arguments are only known at run time.
    Templates { templates: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlannedCall {
    pub id: usize,
    pub line: usize,
    pub col: usize,
    pub tool: String,
    pub effect: &'static str,
    /// The fewest times this site runs when no call fails.
    pub min_count: u64,
    /// The most times it can run; `None` when a loop bound is unknown.
    pub max_count: Option<u64>,
    pub args: Vec<PlannedArg>,
    /// Call sites whose results reach this call's arguments.
    pub data_from: Vec<usize>,
    /// Call sites whose results decide whether or how often this call runs.
    pub control_from: Vec<usize>,
    pub reads: PlannedFootprint,
    /// What the call may write; `None` for pure and read tools.
    pub writes: Option<PlannedFootprint>,
    /// 1 + the deepest stage among `data_from` and `control_from`; `None`
    /// when the site depends on itself through a loop (a sequential chain).
    pub stage: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub calls: Vec<PlannedCall>,
    pub min_calls: u64,
    pub max_calls: Option<u64>,
    /// Number of dependent stages on the longest chain of call sites;
    /// `None` when a loop chains calls on their own results.
    pub stages: Option<usize>,
    /// The strongest effect of any call site (`pure` when there are none).
    pub max_effect: &'static str,
    /// Some call site is reversible or irreversible.
    pub may_write: bool,
}

#[derive(Clone, Copy, Debug)]
struct Mult {
    min: u64,
    max: Option<u64>,
}

impl Mult {
    const ONE: Mult = Mult { min: 1, max: Some(1) };

    fn times(self, other: Mult) -> Mult {
        Mult {
            min: self.min.saturating_mul(other.min),
            max: match (self.max, other.max) {
                (Some(a), Some(b)) => a.checked_mul(b),
                _ => None,
            },
        }
        .fix()
    }

    fn maybe(self) -> Mult {
        Mult { min: 0, max: self.max }
    }

    fn fix(self) -> Mult {
        // A product with a zero bound is zero whatever the other factor.
        if self.max == Some(0) {
            Mult { min: 0, max: Some(0) }
        } else {
            self
        }
    }
}

/// Facts about names that hold for the whole program because the name is
/// bound exactly once.
#[derive(Default)]
struct Consts {
    /// Possible values of a name (literal assignment, loop variable over a
    /// literal list or range, or a copy of such a name).
    values: HashMap<String, Vec<Value>>,
    /// Lists bound once to a literal and changed only by `name += [...]`:
    /// the literal's length and how many such statements there are.
    lists: HashMap<String, ListFacts>,
    /// Names bound once to another name (`xs = tmp`).
    aliases: HashMap<String, String>,
}

#[derive(Clone, Copy)]
struct ListFacts {
    init: u64,
    growth_sites: usize,
}

impl Default for Growth {
    fn default() -> Self {
        Growth { counted: 0, min: 0, max: Some(0), last_top: 0 }
    }
}

/// What the walk has seen of a growing list so far.
#[derive(Clone, Copy)]
struct Growth {
    counted: usize,
    min: u64,
    max: Option<u64>,
    /// Index of the last top-level statement holding a counted site.
    last_top: usize,
}

struct SiteArg {
    name: Option<String>,
    position: Option<usize>,
    /// Call sites whose results reach this argument.
    srcs: BTreeSet<usize>,
    /// Every value it can have, when that is known before the run.
    values: Option<Vec<Value>>,
}

struct Site {
    line: usize,
    col: usize,
    tool: String,
    mult: Mult,
    args: Vec<SiteArg>,
    data: BTreeSet<usize>,
    control: BTreeSet<usize>,
}

struct Planner<'a> {
    tools: &'a ToolRegistry,
    consts: Consts,
    sites: Vec<Site>,
    by_node: HashMap<*const Expr, usize>,
    /// Index of the top-level statement being walked.
    top: usize,
    growth: HashMap<String, Growth>,
    grown: std::collections::HashSet<*const Stmt>,
    /// Length range of an alias, fixed when it was bound.
    alias_lens: HashMap<String, Option<Mult>>,
}

type Env = HashMap<String, BTreeSet<usize>>;

pub fn plan_program(stmts: &[Stmt], tools: &ToolRegistry) -> Plan {
    let mut planner = Planner {
        tools,
        consts: collect_consts(stmts),
        sites: Vec::new(),
        by_node: HashMap::new(),
        top: 0,
        growth: HashMap::new(),
        grown: Default::default(),
        alias_lens: HashMap::new(),
    };
    let mut env = Env::new();
    for (i, stmt) in stmts.iter().enumerate() {
        planner.top = i;
        planner.stmt(stmt, &mut env, Mult::ONE, &BTreeSet::new());
    }
    planner.finish()
}

impl<'a> Planner<'a> {
    fn block(&mut self, stmts: &[Stmt], env: &mut Env, mult: Mult, control: &BTreeSet<usize>) {
        for stmt in stmts {
            self.stmt(stmt, env, mult, control);
        }
    }

    fn stmt(&mut self, stmt: &Stmt, env: &mut Env, mult: Mult, control: &BTreeSet<usize>) {
        match &stmt.kind {
            StmtKind::Assign { targets, value } => {
                if let ([Target::Name { name, .. }], ExprKind::Name(from)) = (targets.as_slice(), &value.kind) {
                    if self.consts.aliases.contains_key(name) && !self.alias_lens.contains_key(name) {
                        let len = self.list_len(from);
                        self.alias_lens.insert(name.clone(), len);
                    }
                }
                let mut src = self.expr(value, env, mult, control);
                src.extend(control.iter().copied());
                for target in targets {
                    self.assign(target, &src, env, mult, control);
                }
            }
            StmtKind::AugAssign { target, value, op } => {
                if let (Target::Name { name, .. }, ExprKind::List(items), crate::ast::BinOp::Add) = (target, &value.kind, op) {
                    if self.consts.lists.contains_key(name) && self.grown.insert(stmt as *const Stmt) {
                        let k = items.len() as u64;
                        let top = self.top;
                        let g = self.growth.entry(name.clone()).or_default();
                        g.counted += 1;
                        g.min = g.min.saturating_add(k.saturating_mul(mult.min));
                        g.max = match (g.max, mult.max) {
                            (Some(a), Some(b)) => b.checked_mul(k).and_then(|x| a.checked_add(x)),
                            _ if k == 0 => g.max,
                            _ => None,
                        };
                        g.last_top = top;
                    }
                }
                let mut src = self.expr(value, env, mult, control);
                src.extend(control.iter().copied());
                src.extend(self.target_reads(target, env, mult, control));
                self.assign(target, &src, env, mult, control);
            }
            StmtKind::Expr(expr) => {
                self.expr(expr, env, mult, control);
            }
            StmtKind::If { test, body, orelse } => {
                let test_src = self.expr(test, env, mult, control);
                let inner: BTreeSet<usize> = control.union(&test_src).copied().collect();
                let (taken_then, taken_else) = match const_truth(test) {
                    Some(true) => (true, false),
                    Some(false) => (false, true),
                    None => (true, true),
                };
                let sure = taken_then != taken_else;
                let mut then_env = env.clone();
                let mut else_env = env.clone();
                if taken_then {
                    let m = if sure { mult } else { mult.maybe() };
                    self.block(body, &mut then_env, m, &inner);
                }
                if taken_else {
                    let m = if sure { mult } else { mult.maybe() };
                    self.block(orelse, &mut else_env, m, &inner);
                }
                *env = join(&then_env, &else_env);
            }
            StmtKind::For { target, iter, body } => {
                let iter_src = self.expr(iter, env, mult, control);
                let trips = self.trips(iter);
                let inner_mult = mult.times(trips);
                let inner: BTreeSet<usize> = control.union(&iter_src).copied().collect();
                // The loop may run zero times, so the state after it joins
                // the state before it; iterate until carried flows settle.
                let before = env.clone();
                let mut state = env.clone();
                loop {
                    let mut round = state.clone();
                    self.assign(target, &iter_src, &mut round, inner_mult, &inner);
                    self.block(body, &mut round, inner_mult, &inner);
                    let next = join(&state, &round);
                    if next == state {
                        break;
                    }
                    state = next;
                }
                *env = join(&before, &state);
            }
            StmtKind::Try { body, handler } => {
                let before = env.clone();
                let first = self.sites.len();
                // A caught error (a tool's or e.g. a KeyError) can skip the
                // rest of the body, so nothing in it is certain to run.
                self.block(body, env, mult.maybe(), control);
                let raised: BTreeSet<usize> = (first..self.sites.len()).collect();
                let mut handler_env = join(&before, env);
                let inner: BTreeSet<usize> = control.union(&raised).copied().collect();
                if let Some(name) = &handler.name {
                    handler_env.insert(name.clone(), inner.clone());
                }
                self.block(&handler.body, &mut handler_env, mult.maybe(), &inner);
                *env = join(env, &handler_env);
            }
            StmtKind::Pass => {}
        }
    }

    fn assign(&mut self, target: &Target, src: &BTreeSet<usize>, env: &mut Env, mult: Mult, control: &BTreeSet<usize>) {
        match target {
            Target::Name { name, .. } => {
                env.insert(name.clone(), src.clone());
            }
            Target::Subscript { base, index, .. } => {
                let idx = self.expr(index, env, mult, control);
                if let Some(name) = root_name(base) {
                    let entry = env.entry(name).or_default();
                    entry.extend(src.iter().copied());
                    entry.extend(idx);
                } else {
                    self.expr(base, env, mult, control);
                }
            }
            Target::Unpack { targets, .. } => {
                for t in targets {
                    self.assign(t, src, env, mult, control);
                }
            }
        }
    }

    fn target_reads(&mut self, target: &Target, env: &mut Env, mult: Mult, control: &BTreeSet<usize>) -> BTreeSet<usize> {
        match target {
            Target::Name { name, .. } => env.get(name).cloned().unwrap_or_default(),
            Target::Subscript { base, index, .. } => {
                let mut s = self.expr(base, env, mult, control);
                s.extend(self.expr(index, env, mult, control));
                s
            }
            Target::Unpack { targets, .. } => {
                let mut s = BTreeSet::new();
                for t in targets {
                    s.extend(self.target_reads(t, env, mult, control));
                }
                s
            }
        }
    }

    /// Evaluate `expr` for its call sites; returns the call sites whose
    /// results can flow into its value.
    fn expr(&mut self, expr: &Expr, env: &mut Env, mult: Mult, control: &BTreeSet<usize>) -> BTreeSet<usize> {
        match &expr.kind {
            ExprKind::None | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) => BTreeSet::new(),
            ExprKind::Name(name) => env.get(name).cloned().unwrap_or_default(),
            ExprKind::List(xs) | ExprKind::Tuple(xs) => self.all(xs, env, mult, control),
            ExprKind::Dict(items) => {
                let mut s = BTreeSet::new();
                for (k, v) in items {
                    s.extend(self.expr(k, env, mult, control));
                    s.extend(self.expr(v, env, mult, control));
                }
                s
            }
            ExprKind::BinOp { left, right, .. } => {
                let mut s = self.expr(left, env, mult, control);
                s.extend(self.expr(right, env, mult, control));
                s
            }
            ExprKind::Unary { operand, .. } => self.expr(operand, env, mult, control),
            ExprKind::BoolOp { values, .. } => {
                // Later operands run only when earlier ones do not decide.
                let mut s = BTreeSet::new();
                let mut inner = control.clone();
                for (i, v) in values.iter().enumerate() {
                    let m = if i == 0 { mult } else { mult.maybe() };
                    let vs = self.expr(v, env, m, &inner);
                    inner.extend(vs.iter().copied());
                    s.extend(vs);
                }
                s
            }
            ExprKind::Compare { left, comparators, .. } => {
                let mut s = self.expr(left, env, mult, control);
                s.extend(self.all(comparators, env, mult, control));
                s
            }
            ExprKind::Subscript { value, index } => {
                let mut s = self.expr(value, env, mult, control);
                s.extend(self.expr(index, env, mult, control));
                s
            }
            ExprKind::Slice { lower, upper, step } => {
                let mut s = BTreeSet::new();
                for e in [lower, upper, step].into_iter().flatten() {
                    s.extend(self.expr(e, env, mult, control));
                }
                s
            }
            ExprKind::FString(segs) => {
                let mut s = BTreeSet::new();
                for seg in segs {
                    if let FSeg::Interp { expr, .. } = seg {
                        s.extend(self.expr(expr, env, mult, control));
                    }
                }
                s
            }
            ExprKind::IfExp { test, body, orelse } => {
                let t = self.expr(test, env, mult, control);
                let inner: BTreeSet<usize> = control.union(&t).copied().collect();
                let mut s = t.clone();
                s.extend(self.expr(body, env, mult.maybe(), &inner));
                s.extend(self.expr(orelse, env, mult.maybe(), &inner));
                s
            }
            ExprKind::Attribute { value, .. } => self.expr(value, env, mult, control),
            ExprKind::Call { func, args, keywords } => self.call(expr, func, args, keywords, env, mult, control),
        }
    }

    fn all(&mut self, xs: &[Expr], env: &mut Env, mult: Mult, control: &BTreeSet<usize>) -> BTreeSet<usize> {
        let mut s = BTreeSet::new();
        for x in xs {
            s.extend(self.expr(x, env, mult, control));
        }
        s
    }

    fn call(
        &mut self,
        node: &Expr,
        func: &Expr,
        args: &[Expr],
        keywords: &[crate::ast::Keyword],
        env: &mut Env,
        mult: Mult,
        control: &BTreeSet<usize>,
    ) -> BTreeSet<usize> {
        let mut arg_srcs = Vec::with_capacity(args.len());
        for a in args {
            arg_srcs.push(self.expr(a, env, mult, control));
        }
        let mut kw_srcs = Vec::with_capacity(keywords.len());
        for k in keywords {
            kw_srcs.push(self.expr(&k.value, env, mult, control));
        }
        let mut all: BTreeSet<usize> = arg_srcs.iter().chain(kw_srcs.iter()).flatten().copied().collect();
        match &func.kind {
            ExprKind::Name(name) if is_builtin(name) || self.tools.get(name).is_none() => {
                // Builtins combine their arguments. An unknown name fails the
                // static check before a plan is made.
                all
            }
            ExprKind::Name(name) => {
                let id = self.site(node, name, args, keywords, &arg_srcs, &kw_srcs, mult, control);
                BTreeSet::from([id])
            }
            ExprKind::Attribute { value, .. } => {
                // A method call (`xs.append(v)`, `d.get(k)`): its arguments
                // flow into the receiver and the result.
                let recv = self.expr(value, env, mult, control);
                if let Some(name) = root_name(value) {
                    let entry = env.entry(name).or_default();
                    entry.extend(all.iter().copied());
                    entry.extend(control.iter().copied());
                }
                all.extend(recv);
                all
            }
            _ => {
                all.extend(self.expr(func, env, mult, control));
                all
            }
        }
    }

    fn site(
        &mut self,
        node: &Expr,
        tool: &str,
        args: &[Expr],
        keywords: &[crate::ast::Keyword],
        arg_srcs: &[BTreeSet<usize>],
        kw_srcs: &[BTreeSet<usize>],
        mult: Mult,
        control: &BTreeSet<usize>,
    ) -> usize {
        let params = self.tools.get(tool).and_then(|t| t.spec.params.clone());
        let mut planned = Vec::new();
        for (i, (a, s)) in args.iter().zip(arg_srcs).enumerate() {
            let name = params.as_ref().and_then(|p| p.params.get(i)).map(|p| p.name.clone());
            planned.push(SiteArg { name, position: Some(i), srcs: s.clone(), values: self.values(a) });
        }
        for (k, s) in keywords.iter().zip(kw_srcs) {
            planned.push(SiteArg { name: k.name.clone(), position: None, srcs: s.clone(), values: self.values(&k.value) });
        }
        let data: BTreeSet<usize> = arg_srcs.iter().chain(kw_srcs.iter()).flatten().copied().collect();
        let key = node as *const Expr;
        if let Some(&id) = self.by_node.get(&key) {
            // A later pass over a loop body: sources can only grow.
            let site = &mut self.sites[id];
            for (old, new) in site.args.iter_mut().zip(planned) {
                old.srcs.extend(new.srcs);
            }
            site.data.extend(data);
            site.control.extend(control.iter().copied());
            return id;
        }
        let id = self.sites.len();
        self.by_node.insert(key, id);
        self.sites.push(Site {
            line: node.line,
            col: node.col,
            tool: tool.to_string(),
            mult,
            args: planned,
            data,
            control: control.clone(),
        });
        id
    }

    /// Every value `expr` can have, when that is a short list known before
    /// the run.
    fn values(&self, expr: &Expr) -> Option<Vec<Value>> {
        if let ExprKind::Name(name) = &expr.kind {
            return self.consts.values.get(name).cloned();
        }
        literal(expr).map(|v| vec![v])
    }

    /// How many times a loop over `iter` runs.
    fn trips(&self, iter: &Expr) -> Mult {
        let exact = |n: u64| Mult { min: n, max: Some(n) };
        let unknown = Mult { min: 0, max: None };
        match &iter.kind {
            ExprKind::List(xs) | ExprKind::Tuple(xs) => exact(xs.len() as u64),
            ExprKind::Str(s) => exact(s.chars().count() as u64),
            ExprKind::Dict(items) => {
                // Duplicate literal keys collapse; count distinct ones when all are literal.
                let keys: Option<Vec<Value>> = items.iter().map(|(k, _)| literal(k)).collect();
                match keys {
                    Some(keys) => {
                        let mut distinct: Vec<Value> = Vec::new();
                        for k in keys {
                            if !distinct.contains(&k) {
                                distinct.push(k);
                            }
                        }
                        exact(distinct.len() as u64)
                    }
                    None => Mult { min: if items.is_empty() { 0 } else { 1 }, max: Some(items.len() as u64) },
                }
            }
            ExprKind::Name(name) => self.list_len(name).unwrap_or(unknown),
            ExprKind::Call { func, args, keywords } if keywords.is_empty() => {
                let ExprKind::Name(f) = &func.kind else { return unknown };
                match f.as_str() {
                    "range" => {
                        let ints: Option<Vec<i64>> = args.iter().map(|a| match literal(a) {
                            Some(Value::Int(i)) => Some(i),
                            _ => None,
                        }).collect();
                        match ints.as_deref() {
                            Some([stop]) => exact(range_len(0, *stop, 1)),
                            Some([start, stop]) => exact(range_len(*start, *stop, 1)),
                            Some([start, stop, step]) if *step != 0 => exact(range_len(*start, *stop, *step)),
                            _ => unknown,
                        }
                    }
                    "enumerate" | "reversed" | "list" | "sorted" if args.len() == 1 => self.trips(&args[0]),
                    _ => unknown,
                }
            }
            _ => unknown,
        }
    }

    /// The length range of list `name` here, when it is known.
    fn list_len(&self, name: &str) -> Option<Mult> {
        if let Some(len) = self.alias_lens.get(name) {
            return *len;
        }
        let facts = self.consts.lists.get(name)?;
        if facts.growth_sites == 0 {
            return Some(Mult { min: facts.init, max: Some(facts.init) });
        }
        let g = self.growth.get(name)?;
        if g.counted != facts.growth_sites {
            // A growth site is still ahead (e.g. inside this very loop).
            return None;
        }
        // Every growth site has been seen, so the total is an upper bound.
        // Its lower bound holds only once those statements have finished.
        let min = if g.last_top < self.top { facts.init.saturating_add(g.min) } else { facts.init };
        Some(Mult { min, max: g.max.and_then(|m| m.checked_add(facts.init)) })
    }

    fn finish(self) -> Plan {
        let n = self.sites.len();
        // Longest chain of dependent sites. A site on a cycle (a loop that
        // feeds a call its own earlier result), or downstream of one, has no
        // stage. Sites off cycles form a DAG, so the memoized walk ends.
        let cyclic = cycle_members(&self.sites);
        let mut memo: Vec<Option<Option<usize>>> = vec![None; n];
        fn depth(i: usize, sites: &[Site], cyclic: &[bool], memo: &mut [Option<Option<usize>>]) -> Option<usize> {
            if let Some(d) = memo[i] {
                return d;
            }
            let result = if cyclic[i] {
                None
            } else {
                let mut best = Some(0usize);
                for &d in sites[i].data.iter().chain(sites[i].control.iter()) {
                    best = match (best, depth(d, sites, cyclic, memo)) {
                        (Some(b), Some(s)) => Some(b.max(s)),
                        _ => None,
                    };
                }
                best.map(|b| b + 1)
            };
            memo[i] = Some(result);
            result
        }
        let finals: Vec<Option<usize>> = (0..n).map(|i| depth(i, &self.sites, &cyclic, &mut memo)).collect();

        let mut calls = Vec::with_capacity(n);
        let mut min_calls = 0u64;
        let mut max_calls = Some(0u64);
        let mut max_effect = Effect::Pure;
        for (id, site) in self.sites.iter().enumerate() {
            let spec = &self.tools.get(&site.tool).expect("planned tool is registered").spec;
            max_effect = max_effect.max(spec.effect);
            min_calls = min_calls.saturating_add(site.mult.min);
            max_calls = match (max_calls, site.mult.max) {
                (Some(a), Some(b)) => a.checked_add(b),
                _ => None,
            };
            let args: Vec<PlannedArg> = site
                .args
                .iter()
                .map(|a| PlannedArg {
                    name: a.name.clone(),
                    position: a.position,
                    source: if !a.srcs.is_empty() {
                        ArgSource::Tools { calls: a.srcs.iter().copied().collect() }
                    } else if let Some(vs) = a.values.as_ref().filter(|vs| vs.len() <= MAX_VALUES) {
                        ArgSource::Literal { values: vs.iter().map(py_repr).collect() }
                    } else {
                        ArgSource::Computed
                    },
                })
                .collect();
            let writes = (spec.effect >= Effect::Reversible).then(|| footprint(&spec.writes, site));
            calls.push(PlannedCall {
                id,
                line: site.line,
                col: site.col,
                tool: site.tool.clone(),
                effect: spec.effect.as_str(),
                min_count: site.mult.min,
                max_count: site.mult.max,
                args,
                data_from: site.data.iter().copied().collect(),
                control_from: site.control.iter().copied().collect(),
                reads: footprint(&spec.reads, site),
                writes,
                stage: finals[id],
            });
        }
        let stages = if finals.iter().any(Option::is_none) { None } else { Some(finals.iter().flatten().copied().max().unwrap_or(0)) };
        Plan {
            calls,
            min_calls,
            max_calls,
            stages,
            max_effect: max_effect.as_str(),
            may_write: max_effect >= Effect::Reversible,
        }
    }
}

fn cycle_members(sites: &[Site]) -> Vec<bool> {
    // i is on a cycle when i is reachable from one of its own dependencies.
    let n = sites.len();
    let mut out = vec![false; n];
    for i in 0..n {
        let mut seen = vec![false; n];
        let mut stack: Vec<usize> = sites[i].data.iter().chain(sites[i].control.iter()).copied().collect();
        while let Some(j) = stack.pop() {
            if j == i {
                out[i] = true;
                break;
            }
            if std::mem::replace(&mut seen[j], true) {
                continue;
            }
            stack.extend(sites[j].data.iter().chain(sites[j].control.iter()).copied());
        }
    }
    out
}

fn footprint(fp: &Footprint, site: &Site) -> PlannedFootprint {
    let Footprint::Only(templates) = fp else {
        return PlannedFootprint::All;
    };
    // Every combination of the possible literal argument values.
    let mut combos: Vec<(Vec<Value>, BTreeMap<String, Value>)> = vec![(Vec::new(), BTreeMap::new())];
    const HOLE: &str = "\u{0}weave-plan-unknown\u{0}";
    for SiteArg { name, position, srcs, values } in &site.args {
        let choices: Vec<Value> = match values {
            Some(vs) if srcs.is_empty() && !vs.is_empty() => vs.clone(),
            _ => vec![Value::Str(HOLE.to_string())],
        };
        if combos.len().saturating_mul(choices.len()) > MAX_NAMES {
            return PlannedFootprint::Templates { templates: templates.clone() };
        }
        let mut next = Vec::with_capacity(combos.len() * choices.len());
        for (a, k) in &combos {
            for c in &choices {
                let (mut a, mut k) = (a.clone(), k.clone());
                match position {
                    Some(_) => {
                        a.push(c.clone());
                        // A positional argument can also fill `{name}`.
                        if let Some(n) = name {
                            k.entry(n.clone()).or_insert_with(|| c.clone());
                        }
                    }
                    None => {
                        if let Some(n) = name {
                            k.insert(n.clone(), c.clone());
                        }
                    }
                }
                next.push((a, k));
            }
        }
        combos = next;
    }
    let mut names: Vec<String> = Vec::new();
    for (a, k) in &combos {
        match fp.resolve(a, k) {
            Footprint::Only(resolved) => {
                if resolved.iter().any(|r| r.contains(HOLE)) {
                    return PlannedFootprint::Templates { templates: templates.clone() };
                }
                for r in resolved {
                    if !names.contains(&r) {
                        names.push(r);
                    }
                }
            }
            Footprint::All => return PlannedFootprint::Templates { templates: templates.clone() },
        }
    }
    PlannedFootprint::Names { names }
}

fn join(a: &Env, b: &Env) -> Env {
    let mut out = a.clone();
    for (k, v) in b {
        out.entry(k.clone()).or_default().extend(v.iter().copied());
    }
    out
}

fn root_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(n) => Some(n.clone()),
        ExprKind::Subscript { value, .. } | ExprKind::Attribute { value, .. } => root_name(value),
        _ => None,
    }
}

fn range_len(start: i64, stop: i64, step: i64) -> u64 {
    let (start, stop, step) = (start as i128, stop as i128, step as i128);
    let n = if step > 0 {
        if stop > start { (stop - start + step - 1) / step } else { 0 }
    } else if start > stop {
        (start - stop + (-step) - 1) / (-step)
    } else {
        0
    };
    n.clamp(0, u64::MAX as i128) as u64
}

/// The value of a constant expression made only of literals.
fn literal(expr: &Expr) -> Option<Value> {
    Some(match &expr.kind {
        ExprKind::None => Value::Null,
        ExprKind::Bool(b) => Value::Bool(*b),
        ExprKind::Int(i) => Value::Int(*i),
        ExprKind::Float(f) => Value::Float(*f),
        ExprKind::Str(s) => Value::Str(s.clone()),
        ExprKind::List(xs) | ExprKind::Tuple(xs) => Value::List(xs.iter().map(literal).collect::<Option<_>>()?),
        ExprKind::Dict(items) => Value::Dict(
            items.iter().map(|(k, v)| Some((literal(k)?, literal(v)?))).collect::<Option<_>>()?,
        ),
        ExprKind::Unary { op: UnaryOp::USub, operand } => match literal(operand)? {
            Value::Int(i) => Value::Int(i.checked_neg()?),
            Value::Float(f) => Value::Float(-f),
            _ => return None,
        },
        ExprKind::FString(segs) if segs.iter().all(|s| matches!(s, FSeg::Lit(_))) => Value::Str(
            segs.iter().map(|s| if let FSeg::Lit(t) = s { t.as_str() } else { "" }).collect(),
        ),
        _ => return None,
    })
}

fn const_truth(expr: &Expr) -> Option<bool> {
    match literal(expr)? {
        Value::Null => Some(false),
        Value::Bool(b) => Some(b),
        Value::Int(i) => Some(i != 0),
        Value::Float(f) => Some(f != 0.0),
        Value::Str(s) => Some(!s.is_empty()),
        Value::List(xs) => Some(!xs.is_empty()),
        Value::Dict(xs) => Some(!xs.is_empty()),
    }
}

/// Names bound exactly once in the whole program, and what is known about
/// them. A name bound twice, mutated through a method, or assigned through a
/// subscript gets no facts. `name += [...]` is not a binding: it marks a
/// growing list, whose value is never treated as constant.
fn collect_consts(stmts: &[Stmt]) -> Consts {
    let mut bindings: HashMap<String, Vec<Binding>> = HashMap::new();
    let mut grows: HashMap<String, usize> = HashMap::new();
    let mut mutated: BTreeSet<String> = BTreeSet::new();
    walk_bindings(stmts, &mut bindings, &mut grows, &mut mutated, false);
    let mut consts = Consts::default();
    let once = |name: &str| bindings.get(name).map(Vec::len) == Some(1) && !mutated.contains(name);
    for (name, binds) in &bindings {
        if !once(name) {
            continue;
        }
        let growth_sites = grows.get(name).copied().unwrap_or(0);
        match &binds[0] {
            Binding::Value { value, top } => {
                if let Value::List(xs) = value {
                    if growth_sites == 0 || *top {
                        consts.lists.insert(name.clone(), ListFacts { init: xs.len() as u64, growth_sites });
                    }
                }
                if growth_sites == 0 {
                    consts.values.insert(name.clone(), vec![value.clone()]);
                }
            }
            Binding::LoopOver(items) if growth_sites == 0 => {
                consts.values.insert(name.clone(), items.clone());
            }
            Binding::Alias(from) if growth_sites == 0 => {
                consts.aliases.insert(name.clone(), from.clone());
            }
            _ => {}
        }
    }
    // Copies and loop variables over once-bound names, followed through a
    // few levels of copies.
    for _ in 0..4 {
        for (name, binds) in &bindings {
            if !once(name) || grows.contains_key(name) || consts.values.contains_key(name) {
                continue;
            }
            match &binds[0] {
                Binding::Alias(from) => {
                    if let Some(v) = consts.values.get(from).cloned() {
                        consts.values.insert(name.clone(), v);
                    }
                }
                Binding::LoopOverName(list) => {
                    if let Some([Value::List(items)]) = consts.values.get(list).map(Vec::as_slice) {
                        let items = distinct(items.clone());
                        consts.values.insert(name.clone(), items);
                    }
                }
                _ => {}
            }
        }
    }
    consts
}

enum Binding {
    Value { value: Value, top: bool },
    LoopOver(Vec<Value>),
    LoopOverName(String),
    Alias(String),
    Other,
}

fn distinct(items: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// The values of `for x in range(...)` with literal bounds, when there are
/// only a few.
fn range_values(iter: &Expr) -> Option<Vec<Value>> {
    let ExprKind::Call { func, args, keywords } = &iter.kind else { return None };
    if !keywords.is_empty() || !matches!(&func.kind, ExprKind::Name(n) if n == "range") {
        return None;
    }
    let ints: Vec<i64> = args.iter().map(|a| match literal(a) {
        Some(Value::Int(i)) => Some(i),
        _ => None,
    }).collect::<Option<_>>()?;
    let (start, stop, step) = match ints.as_slice() {
        [stop] => (0, *stop, 1),
        [start, stop] => (*start, *stop, 1),
        [start, stop, step] if *step != 0 => (*start, *stop, *step),
        _ => return None,
    };
    if range_len(start, stop, step) > MAX_NAMES as u64 {
        return None;
    }
    let mut out = Vec::new();
    let mut i = start;
    while (step > 0 && i < stop) || (step < 0 && i > stop) {
        out.push(Value::Int(i));
        i = i.checked_add(step)?;
    }
    Some(out)
}

fn walk_bindings(
    stmts: &[Stmt],
    out: &mut HashMap<String, Vec<Binding>>,
    grows: &mut HashMap<String, usize>,
    mutated: &mut BTreeSet<String>,
    nested: bool,
) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { targets, value } => {
                for t in targets {
                    match t {
                        Target::Name { name, .. } if targets.len() == 1 => {
                            let b = match (&value.kind, literal(value)) {
                                (_, Some(v)) => Binding::Value { value: v, top: !nested },
                                (ExprKind::Name(from), None) => Binding::Alias(from.clone()),
                                _ => Binding::Other,
                            };
                            out.entry(name.clone()).or_default().push(b);
                        }
                        _ => bind_other(t, out, mutated),
                    }
                }
                mark_mutations(value, mutated);
            }
            StmtKind::AugAssign { target, value, op } => {
                match (target, &value.kind, op) {
                    (Target::Name { name, .. }, ExprKind::List(_), crate::ast::BinOp::Add) => {
                        *grows.entry(name.clone()).or_default() += 1;
                    }
                    _ => bind_other(target, out, mutated),
                }
                mark_mutations(value, mutated);
            }
            StmtKind::Expr(e) => mark_mutations(e, mutated),
            StmtKind::If { test, body, orelse } => {
                mark_mutations(test, mutated);
                walk_bindings(body, out, grows, mutated, true);
                walk_bindings(orelse, out, grows, mutated, true);
            }
            StmtKind::For { target, iter, body } => {
                mark_mutations(iter, mutated);
                match target {
                    Target::Name { name, .. } => {
                        let b = match (&iter.kind, literal(iter)) {
                            (_, Some(Value::List(items))) => Binding::LoopOver(distinct(items)),
                            (ExprKind::Name(list), _) => Binding::LoopOverName(list.clone()),
                            _ => match range_values(iter) {
                                Some(items) => Binding::LoopOver(items),
                                None => Binding::Other,
                            },
                        };
                        out.entry(name.clone()).or_default().push(b);
                    }
                    _ => bind_other(target, out, mutated),
                }
                walk_bindings(body, out, grows, mutated, true);
            }
            StmtKind::Try { body, handler } => {
                walk_bindings(body, out, grows, mutated, true);
                if let Some(name) = &handler.name {
                    out.entry(name.clone()).or_default().push(Binding::Other);
                }
                walk_bindings(&handler.body, out, grows, mutated, true);
            }
            StmtKind::Pass => {}
        }
    }
}

fn bind_other(t: &Target, out: &mut HashMap<String, Vec<Binding>>, mutated: &mut BTreeSet<String>) {
    match t {
        Target::Name { name, .. } => out.entry(name.clone()).or_default().push(Binding::Other),
        Target::Subscript { base, .. } => {
            if let Some(n) = root_name(base) {
                mutated.insert(n);
            }
        }
        Target::Unpack { targets, .. } => {
            for t in targets {
                bind_other(t, out, mutated);
            }
        }
    }
}

/// Any `x.method(...)` may change `x`.
fn mark_mutations(expr: &Expr, mutated: &mut BTreeSet<String>) {
    visit_expr(expr, &mut |e| {
        if let ExprKind::Call { func, .. } = &e.kind {
            if let ExprKind::Attribute { value, .. } = &func.kind {
                if let Some(n) = root_name(value) {
                    mutated.insert(n);
                }
            }
        }
    });
}

fn visit_expr(expr: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(expr);
    match &expr.kind {
        ExprKind::List(xs) | ExprKind::Tuple(xs) => xs.iter().for_each(|x| visit_expr(x, f)),
        ExprKind::Dict(items) => items.iter().for_each(|(k, v)| {
            visit_expr(k, f);
            visit_expr(v, f);
        }),
        ExprKind::BinOp { left, right, .. } => {
            visit_expr(left, f);
            visit_expr(right, f);
        }
        ExprKind::Unary { operand, .. } => visit_expr(operand, f),
        ExprKind::BoolOp { values, .. } => values.iter().for_each(|x| visit_expr(x, f)),
        ExprKind::Compare { left, comparators, .. } => {
            visit_expr(left, f);
            comparators.iter().for_each(|x| visit_expr(x, f));
        }
        ExprKind::Call { func, args, keywords } => {
            visit_expr(func, f);
            args.iter().for_each(|x| visit_expr(x, f));
            keywords.iter().for_each(|k| visit_expr(&k.value, f));
        }
        ExprKind::Attribute { value, .. } => visit_expr(value, f),
        ExprKind::Subscript { value, index } => {
            visit_expr(value, f);
            visit_expr(index, f);
        }
        ExprKind::Slice { lower, upper, step } => [lower, upper, step].into_iter().flatten().for_each(|x| visit_expr(x, f)),
        ExprKind::FString(segs) => segs.iter().for_each(|s| {
            if let FSeg::Interp { expr, .. } = s {
                visit_expr(expr, f)
            }
        }),
        ExprKind::IfExp { test, body, orelse } => {
            visit_expr(test, f);
            visit_expr(body, f);
            visit_expr(orelse, f);
        }
        _ => {}
    }
}

impl Plan {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("plan serializes")
    }

    /// A short human-readable summary, one line per call site.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let count = match self.max_calls {
            Some(max) if max == self.min_calls => format!("{max} tool call{}", if max == 1 { "" } else { "s" }),
            Some(max) => format!("{} to {max} tool calls", self.min_calls),
            None => format!("at least {} tool calls (a loop bound is only known at run time)", self.min_calls),
        };
        let stages = match self.stages {
            Some(s) => format!("{s} dependent stage{}", if s == 1 { "" } else { "s" }),
            None => "a loop that chains calls on their own results".to_string(),
        };
        out.push_str(&format!("plan: {count}, {stages}, strongest effect {}\n", self.max_effect));
        for c in &self.calls {
            let times = match (c.min_count, c.max_count) {
                (a, Some(b)) if a == b => format!("x{a}"),
                (a, Some(b)) => format!("x{a}-{b}"),
                (a, None) => format!("x{a}+"),
            };
            let args: Vec<String> = c
                .args
                .iter()
                .map(|a| {
                    let v = match &a.source {
                        ArgSource::Literal { values } if values.len() == 1 => values[0].clone(),
                        ArgSource::Literal { values } => values.join(" | "),
                        ArgSource::Tools { calls } => format!("<{}>", calls.iter().map(|i| format!("#{}", i + 1)).collect::<Vec<_>>().join(",")),
                        ArgSource::Computed => "<computed>".to_string(),
                    };
                    match (&a.name, a.position) {
                        (Some(n), None) => format!("{n}={v}"),
                        _ => v,
                    }
                })
                .collect();
            let stage = match c.stage {
                Some(s) => format!("stage {s}"),
                None => "chained".to_string(),
            };
            out.push_str(&format!("  #{} line {}: {}({}) {} {} {}", c.id + 1, c.line, c.tool, args.join(", "), c.effect, times, stage));
            if c.effect == "reversible" || c.effect == "irreversible" {
                let w = match c.writes.as_ref().unwrap_or(&PlannedFootprint::All) {
                    PlannedFootprint::All => "anything (no writes= declared)".to_string(),
                    PlannedFootprint::Names { names } => names.join(", "),
                    PlannedFootprint::Templates { templates } => format!("{} (filled at run time)", templates.join(", ")),
                };
                out.push_str(&format!(", writes {w}"));
            }
            out.push('\n');
        }
        out
    }
}
