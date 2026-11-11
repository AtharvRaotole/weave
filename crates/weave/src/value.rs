use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::json;

/// Runtime value. Dicts keep insertion order. Equality of dicts ignores order.
#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Dict(Vec<(Value, Value)>),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Int(a), Self::Float(b)) | (Self::Float(b), Self::Int(a)) => {
                (*a as f64) == *b && b.is_finite()
            }
            (Self::Float(a), Self::Float(b)) => a == b,
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::List(a), Self::List(b)) => a == b,
            (Self::Dict(a), Self::Dict(b)) => dict_eq(a, b),
            _ => false,
        }
    }
}

fn dict_eq(a: &[(Value, Value)], b: &[(Value, Value)]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().all(|(k, v)| dict_get(b, k).is_some_and(|ov| ov == v))
}

pub fn dict_get<'a>(items: &'a [(Value, Value)], key: &Value) -> Option<&'a Value> {
    items.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
}

impl Value {
    pub fn str(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }

    pub fn int(i: i64) -> Self {
        Self::Int(i)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Bool(b) => Some(if *b { 1 } else { 0 }),
            _ => None,
        }
    }
}

pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0 && !f.is_nan(),
        Value::Str(s) => !s.is_empty(),
        Value::List(xs) => !xs.is_empty(),
        Value::Dict(xs) => !xs.is_empty(),
    }
}

pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format_float(*f),
        Value::Str(s) => s.clone(),
        Value::List(_) | Value::Dict(_) => py_repr(value),
    }
}

pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format_float(*f),
        Value::Str(s) => repr_str(s),
        Value::List(xs) => {
            let parts: Vec<String> = xs.iter().map(py_repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Dict(xs) => {
            let parts: Vec<String> = xs
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr(k), py_repr(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f.is_sign_negative() {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    // CPython's repr: shortest round-trip digits, scientific notation when
    // the decimal exponent is below -4 or at least 16.
    let sci = format!("{f:e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if f != 0.0 && !(-4..16).contains(&exp) {
        return format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let s = format!("{f}");
    if s.contains(['.', 'e', 'E']) {
        s
    } else {
        format!("{s}.0")
    }
}

fn repr_str(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", py_repr(self))
    }
}

pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "None",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "str",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
    }
}

fn is_numeric(value: &Value) -> bool {
    matches!(value, Value::Bool(_) | Value::Int(_) | Value::Float(_))
}

fn as_number(value: &Value) -> Result<Num, String> {
    match value {
        Value::Bool(b) => Ok(Num::Int(if *b { 1 } else { 0 })),
        Value::Int(i) => Ok(Num::Int(*i)),
        Value::Float(f) if f.is_finite() => Ok(Num::Float(*f)),
        Value::Float(_) => Err("float is not finite".into()),
        other => Err(format!(
            "expected a number, got {}",
            describe(other)
        )),
    }
}

/// A type name for error messages. Dicts list their first keys, so a
/// script that treats a `{"value": ...}` result as a plain value learns
/// which key to read.
pub fn describe(value: &Value) -> String {
    match value {
        Value::Dict(items) if !items.is_empty() => {
            let keys: Vec<String> = items.iter().take(8).map(|(k, _)| py_repr(k)).collect();
            let more = if items.len() > 8 { ", ..." } else { "" };
            format!("dict with keys {}{more}", keys.join(", "))
        }
        other => type_name(other).to_string(),
    }
}

enum Num {
    Int(i64),
    Float(f64),
}

fn checked_i64(label: &str, v: i128) -> Result<i64, String> {
    i64::try_from(v).map_err(|_| format!("{label} overflows int"))
}

pub fn add(a: &Value, b: &Value) -> Result<Value, String> {
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => Ok(Value::Str(format!("{x}{y}"))),
        (Value::List(x), Value::List(y)) => {
            let mut out = x.clone();
            out.extend(y.iter().cloned());
            Ok(Value::List(out))
        }
        _ if !is_numeric(a) || !is_numeric(b) => Err(format!(
            "cannot add {} and {}",
            describe(a),
            describe(b)
        )),
        _ => {
            let (a, b) = (as_number(a)?, as_number(b)?);
            match (a, b) {
                (Num::Int(x), Num::Int(y)) => {
                    Ok(Value::Int(checked_i64("addition", x as i128 + y as i128)?))
                }
                (x, y) => {
                    let xf = match x {
                        Num::Int(i) => i as f64,
                        Num::Float(f) => f,
                    };
                    let yf = match y {
                        Num::Int(i) => i as f64,
                        Num::Float(f) => f,
                    };
                    Ok(Value::Float(xf + yf))
                }
            }
        }
    }
}

pub fn sub(a: &Value, b: &Value) -> Result<Value, String> {
    let (a, b) = (as_number(a)?, as_number(b)?);
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => Ok(Value::Int(checked_i64("subtraction", x as i128 - y as i128)?)),
        (x, y) => Ok(Value::Float(to_f(x) - to_f(y))),
    }
}

pub fn mul(a: &Value, b: &Value) -> Result<Value, String> {
    match (a, b) {
        (Value::Str(s), Value::Int(n)) | (Value::Int(n), Value::Str(s)) => Ok(Value::Str(repeat_str(s, *n)?)),
        (Value::List(xs), Value::Int(n)) | (Value::Int(n), Value::List(xs)) => {
            Ok(Value::List(repeat_list(xs, *n)?))
        }
        _ => {
            let (a, b) = (as_number(a)?, as_number(b)?);
            match (a, b) {
                (Num::Int(x), Num::Int(y)) => {
                    Ok(Value::Int(checked_i64("multiplication", x as i128 * y as i128)?))
                }
                (x, y) => Ok(Value::Float(to_f(x) * to_f(y))),
            }
        }
    }
}

/// Refuse a repetition whose result would pass the hard size cap, before
/// allocating it.
fn repeat_len(unit: usize, n: i64) -> Result<usize, String> {
    let total = (unit as u128).saturating_mul(n.max(0) as u128);
    if total > crate::limits::HARD_MAX_VALUE_SIZE as u128 {
        return Err(format!(
            "repetition would build a value of {total} items; the limit is {}",
            crate::limits::HARD_MAX_VALUE_SIZE
        ));
    }
    Ok(total as usize)
}

fn repeat_str(s: &str, n: i64) -> Result<String, String> {
    repeat_len(s.len(), n)?;
    if n <= 0 {
        return Ok(String::new());
    }
    Ok(s.repeat(n as usize))
}

fn repeat_list(xs: &[Value], n: i64) -> Result<Vec<Value>, String> {
    let cap = crate::limits::HARD_MAX_VALUE_SIZE;
    let unit = xs
        .iter()
        .fold(xs.len(), |acc, x| acc.saturating_add(crate::limits::size_capped(x, cap)).min(cap + 1));
    repeat_len(unit, n)?;
    let total = xs.len().saturating_mul(n.max(0) as usize);
    if n <= 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(total);
    for _ in 0..n {
        out.extend(xs.iter().cloned());
    }
    Ok(out)
}

fn to_f(n: Num) -> f64 {
    match n {
        Num::Int(i) => i as f64,
        Num::Float(f) => f,
    }
}

pub fn true_div(a: &Value, b: &Value) -> Result<Value, String> {
    let x = to_f(as_number(a)?);
    let y = to_f(as_number(b)?);
    if y == 0.0 {
        return Err("division by zero".into());
    }
    Ok(Value::Float(x / y))
}

pub fn floor_div(a: &Value, b: &Value) -> Result<Value, String> {
    match (as_number(a)?, as_number(b)?) {
        (Num::Int(x), Num::Int(y)) => {
            if y == 0 {
                return Err("division by zero".into());
            }
            if x == i64::MIN && y == -1 {
                return Err("integer overflow in //".into());
            }
            let q = x / y;
            Ok(Value::Int(if x % y != 0 && ((x < 0) != (y < 0)) { q - 1 } else { q }))
        }
        (x, y) => {
            let y = to_f(y);
            if y == 0.0 {
                return Err("division by zero".into());
            }
            Ok(Value::Float((to_f(x) / y).floor()))
        }
    }
}

pub fn modulo(a: &Value, b: &Value) -> Result<Value, String> {
    match (a, b) {
        (Value::Str(s), other) => Ok(Value::Str(format_percent(s, other)?)),
        _ => match (as_number(a)?, as_number(b)?) {
            (Num::Int(x), Num::Int(y)) => {
                if y == 0 {
                    return Err("division by zero".into());
                }
                let r = x.wrapping_rem(y);
                Ok(Value::Int(if r != 0 && ((r < 0) != (y < 0)) { r + y } else { r }))
            }
            (x, y) => {
                let y = to_f(y);
                if y == 0.0 {
                    return Err("division by zero".into());
                }
                let x = to_f(x);
                let r = x % y;
                Ok(Value::Float(if r != 0.0 && ((r < 0.0) != (y < 0.0)) { r + y } else if r == 0.0 { 0.0f64.copysign(y) } else { r }))
            }
        },
    }
}

fn format_percent(fmt: &str, value: &Value) -> Result<String, String> {
    // printf-style: `%[flags][width][.precision]type` with flags `-+ 0` and
    // types `s r d i f F e E x X o %`. A list on the right supplies one value
    // per conversion (Weave has no tuples), unless there is exactly one.
    let chars: Vec<char> = fmt.chars().collect();
    let mut specs: Vec<(String, char)> = Vec::new();
    let mut pieces: Vec<Option<String>> = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            lit.push(chars[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i < chars.len() && chars[i] == '%' {
            lit.push('%');
            i += 1;
            continue;
        }
        if i < chars.len() && chars[i] == '(' {
            return Err("`%(name)s` formatting is not supported; use an f-string".into());
        }
        let mut flags = String::new();
        while i < chars.len() && "-+ 0#".contains(chars[i]) {
            flags.push(chars[i]);
            i += 1;
        }
        let mut width = String::new();
        while i < chars.len() && chars[i].is_ascii_digit() {
            width.push(chars[i]);
            i += 1;
        }
        let mut prec = String::new();
        if i < chars.len() && chars[i] == '.' {
            prec.push('.');
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                prec.push(chars[i]);
                i += 1;
            }
            if prec == "." {
                prec.push('0');
            }
        }
        let Some(&ty) = chars.get(i) else {
            return Err("incomplete format: a `%` at the end".into());
        };
        i += 1;
        if !"srdifFeExXo".contains(ty) || flags.contains('#') || width.contains('*') {
            return Err(format!("`%{flags}{width}{prec}{ty}` formatting is not supported; use an f-string"));
        }
        pieces.push(Some(std::mem::take(&mut lit)));
        let mut spec = String::new();
        if flags.contains('-') {
            spec.push('<');
        } else if ty == 's' || ty == 'r' {
            spec.push('>');
        }
        if flags.contains('+') {
            spec.push('+');
        } else if flags.contains(' ') {
            spec.push(' ');
        }
        if flags.contains('0') && !flags.contains('-') && ty != 's' && ty != 'r' {
            spec.push('0');
        }
        specs.push((format!("{spec}{width}{prec}"), ty));
        pieces.push(None);
    }
    pieces.push(Some(lit));
    let values: Vec<Value> = match (specs.len(), value) {
        (1, v) => vec![v.clone()],
        (_, Value::List(xs)) => xs.clone(),
        (0, _) => return Err("not all arguments converted during string formatting".into()),
        (_, v) => vec![v.clone()],
    };
    if values.len() != specs.len() {
        return Err(format!(
            "format string has {} conversions but got {} values",
            specs.len(),
            values.len()
        ));
    }
    let mut out = String::new();
    let mut k = 0;
    for piece in pieces {
        match piece {
            Some(text) => out.push_str(&text),
            None => {
                let (spec, ty) = &specs[k];
                let v = &values[k];
                k += 1;
                let text = match ty {
                    's' => format_value(&Value::Str(py_str(v)), spec)?,
                    'r' => format_value(&Value::Str(py_repr(v)), spec)?,
                    'd' | 'i' => {
                        let n = match v {
                            Value::Int(n) => *n,
                            Value::Bool(b) => *b as i64,
                            Value::Float(f) if f.is_finite() => f.trunc() as i64,
                            other => return Err(format!("%d format: a number is required, not {}", type_name(other))),
                        };
                        let spec = spec.split('.').next().unwrap_or("").to_string();
                        format_value(&Value::Int(n), &format!("{spec}d"))?
                    }
                    t => {
                        let v = match (t, v) {
                            ('x' | 'X' | 'o', Value::Float(_)) => return Err(format!("%{t} format: an int is required, not float")),
                            ('x' | 'X' | 'o', _) => v.clone(),
                            (_, Value::Int(n)) => Value::Float(*n as f64),
                            _ => v.clone(),
                        };
                        format_value(&v, &format!("{spec}{t}"))?
                    }
                };
                out.push_str(&text);
            }
        }
    }
    Ok(out)
}

pub fn pow(a: &Value, b: &Value) -> Result<Value, String> {
    match (as_number(a)?, as_number(b)?) {
        (Num::Int(base), Num::Int(exp)) => {
            if exp < 0 {
                if base == 0 {
                    return Err("zero to a negative power".into());
                }
                return Ok(Value::Float((base as f64).powf(exp as f64)));
            }
            if exp > 10_000 {
                return Err("exponent is too large".into());
            }
            let mut acc: i128 = 1;
            let mut bse: i128 = base as i128;
            let mut e = exp as u64;
            while e > 0 {
                if e & 1 == 1 {
                    acc = acc
                        .checked_mul(bse)
                        .ok_or_else(|| "power overflows int".to_string())?;
                }
                e >>= 1;
                if e > 0 {
                    bse = bse
                        .checked_mul(bse)
                        .ok_or_else(|| "power overflows int".to_string())?;
                }
            }
            Ok(Value::Int(checked_i64("power", acc)?))
        }
        (x, y) => Ok(Value::Float(to_f(x).powf(to_f(y)))),
    }
}

pub fn neg(a: &Value) -> Result<Value, String> {
    match as_number(a)? {
        Num::Int(i) => Ok(Value::Int(checked_i64("negation", -(i as i128))?)),
        Num::Float(f) => Ok(Value::Float(-f)),
    }
}

pub fn bit_not(a: &Value) -> Result<Value, String> {
    match a.as_int() {
        Some(i) => Ok(Value::Int(!i)),
        None => Err(format!("~ expects int, got {}", type_name(a))),
    }
}

pub fn bit_op(op: &str, a: &Value, b: &Value) -> Result<Value, String> {
    let x = a.as_int().ok_or_else(|| format!("{op} expects int, got {}", type_name(a)))?;
    let y = b.as_int().ok_or_else(|| format!("{op} expects int, got {}", type_name(b)))?;
    let v = match op {
        "&" => x & y,
        "|" => x | y,
        "^" => x ^ y,
        "<<" => {
            if !(0..=63).contains(&y) {
                return Err("shift is out of range".into());
            }
            x.checked_shl(y as u32).ok_or("shift overflows int")?
        }
        ">>" => {
            if !(0..=63).contains(&y) {
                return Err("shift is out of range".into());
            }
            x.checked_shr(y as u32).ok_or("shift overflows int")?
        }
        _ => return Err(format!("operator `{op}` is not supported")),
    };
    Ok(Value::Int(v))
}

#[derive(Clone, Copy)]
pub enum Cmp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    In,
    NotIn,
    Is,
    IsNot,
}

pub fn compare(op: Cmp, left: &Value, right: &Value) -> Result<Value, String> {
    let ok = match op {
        Cmp::Eq => left == right,
        Cmp::NotEq => left != right,
        Cmp::Is => is_identical(left, right)?,
        Cmp::IsNot => !is_identical(left, right)?,
        Cmp::In => contains(right, left)?,
        Cmp::NotIn => !contains(right, left)?,
        Cmp::Lt | Cmp::LtE | Cmp::Gt | Cmp::GtE => cmp_ord(left, right, op)?,
    };
    Ok(Value::Bool(ok))
}

fn is_identical(left: &Value, right: &Value) -> Result<bool, String> {
    match (left, right) {
        (Value::Null, Value::Null) => Ok(true),
        (Value::Bool(a), Value::Bool(b)) => Ok(a == b),
        (Value::Null, _) | (_, Value::Null) => Ok(false),
        (Value::Bool(_), _) | (_, Value::Bool(_)) => Ok(false),
        _ => Err("`is` is only supported for None and bool; use == for values".into()),
    }
}

fn contains(container: &Value, item: &Value) -> Result<bool, String> {
    match container {
        Value::Str(s) => {
            let Value::Str(part) = item else {
                return Err("`in` on a str expects a str".into());
            };
            Ok(s.contains(part))
        }
        Value::List(xs) => Ok(xs.iter().any(|v| v == item)),
        Value::Dict(xs) => Ok(dict_get(xs, item).is_some()),
        other => Err(format!("`in` expects str, list, or dict, got {}", type_name(other))),
    }
}

fn cmp_ord(left: &Value, right: &Value, op: Cmp) -> Result<bool, String> {
    let ord = ordering(left, right).map_err(|_| {
        format!("cannot compare {} and {} with `{}`", type_name(left), type_name(right), cmp_token(op))
    })?;
    Ok(match op {
        Cmp::Lt => ord.is_lt(),
        Cmp::LtE => ord.is_le(),
        Cmp::Gt => ord.is_gt(),
        Cmp::GtE => ord.is_ge(),
        _ => unreachable!(),
    })
}

/// Python ordering: numbers, strings and bools among themselves, and lists
/// lexicographically (so `sorted` of `[key, item]` pairs works).
fn ordering(left: &Value, right: &Value) -> Result<std::cmp::Ordering, ()> {
    Ok(match (left, right) {
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::Str(a), Value::Str(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::List(a), Value::List(b)) => {
            for (x, y) in a.iter().zip(b) {
                let o = ordering(x, y)?;
                if o.is_ne() {
                    return Ok(o);
                }
            }
            a.len().cmp(&b.len())
        }
        _ => match (as_number(left), as_number(right)) {
            (Ok(a), Ok(b)) => to_f(a).partial_cmp(&to_f(b)).ok_or(())?,
            _ => return Err(()),
        },
    })
}

fn cmp_token(op: Cmp) -> &'static str {
    match op {
        Cmp::Lt => "<",
        Cmp::LtE => "<=",
        Cmp::Gt => ">",
        Cmp::GtE => ">=",
        _ => "?",
    }
}

pub fn len_of(value: &Value) -> Result<Value, String> {
    let n = match value {
        Value::Str(s) => s.chars().count(),
        Value::List(xs) => xs.len(),
        Value::Dict(xs) => xs.len(),
        other => return Err(format!("len() expects str, list, or dict, got {}", type_name(other))),
    };
    Ok(Value::Int(n as i64))
}

pub fn get_item(container: &Value, index: &Value) -> Result<Value, String> {
    match container {
        Value::List(xs) => {
            let i = index.as_int().ok_or_else(|| {
                let first = xs.first().map(|x| format!("; the first item is a {}", describe(x))).unwrap_or_default();
                format!(
                    "cannot look up {} in a list of {} items{first}; loop over the list or index it with an int",
                    py_repr(index),
                    xs.len()
                )
            })?;
            let idx = normalize_index(i, xs.len()).map_err(|_| format!("list index {i} is out of range for a list of {} items (index out of range)", xs.len()))?;
            Ok(xs[idx].clone())
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let i = index.as_int().ok_or_else(|| {
                let preview: String = s.chars().take(60).collect();
                let more = if s.chars().count() > 60 { "..." } else { "" };
                format!(
                    "cannot look up {} in a str: this value is the str {}{more}, not a dict; print it to see its shape, or parse_json() it if it holds JSON",
                    py_repr(index),
                    py_repr(&Value::Str(preview))
                )
            })?;
            let idx = normalize_index(i, chars.len()).map_err(|_| format!("string index {i} is out of range for a str of {} characters (index out of range)", chars.len()))?;
            Ok(Value::Str(chars[idx].to_string()))
        }
        Value::Dict(xs) => dict_get(xs, index)
            .cloned()
            .ok_or_else(|| match describe(container) {
                d if d.starts_with("dict with keys") => format!("key {} is missing; the dict has keys {}", py_repr(index), &d[15..]),
                _ => format!("key {} is missing; the dict is empty", py_repr(index)),
            }),
        other => Err(format!("cannot look up {} in {}; only lists, dicts and strs can be indexed", py_repr(index), type_name(other))),
    }
}

pub fn get_slice(container: &Value, lower: Option<&Value>, upper: Option<&Value>, step: Option<&Value>) -> Result<Value, String> {
    let step_i = match step {
        None => 1,
        Some(v) => v.as_int().ok_or("slice step must be int")?,
    };
    if step_i == 0 {
        return Err("slice step cannot be zero".into());
    }
    match container {
        Value::List(xs) => {
            let idxs = slice_indices(xs.len(), lower, upper, step_i)?;
            Ok(Value::List(idxs.into_iter().map(|i| xs[i].clone()).collect()))
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let idxs = slice_indices(chars.len(), lower, upper, step_i)?;
            Ok(Value::Str(idxs.into_iter().map(|i| chars[i]).collect()))
        }
        other => Err(format!("cannot slice {}", type_name(other))),
    }
}

fn slice_indices(len: usize, lower: Option<&Value>, upper: Option<&Value>, step: i64) -> Result<Vec<usize>, String> {
    let len_i = len as i64;
    let (start, end) = if step > 0 {
        let start = bound(lower, 0, len_i, len_i)?;
        let end = bound(upper, len_i, len_i, len_i)?;
        (start, end)
    } else {
        let start = bound(lower, len_i - 1, len_i, len_i)?;
        let end = bound(upper, -1, len_i, len_i)?;
        (start, end)
    };
    let mut out = Vec::new();
    let mut i = start;
    if step > 0 {
        while i < end {
            if i >= 0 && i < len_i {
                out.push(i as usize);
            }
            i += step;
        }
    } else {
        while i > end {
            if i >= 0 && i < len_i {
                out.push(i as usize);
            }
            i += step;
        }
    }
    Ok(out)
}

fn bound(v: Option<&Value>, default: i64, len: i64, _len2: i64) -> Result<i64, String> {
    let Some(v) = v else {
        return Ok(default);
    };
    if v.is_null() {
        return Ok(default);
    }
    let mut i = v.as_int().ok_or("slice bound must be int or None")?;
    if i < 0 {
        i += len;
    }
    Ok(i.clamp(if default < 0 { -1 } else { 0 }, len))
}

fn normalize_index(i: i64, len: usize) -> Result<usize, String> {
    let mut i = i;
    let len = len as i64;
    if i < 0 {
        i += len;
    }
    if i < 0 || i >= len {
        return Err("index out of range".into());
    }
    Ok(i as usize)
}

pub fn set_item(container: &Value, index: &Value, value: Value) -> Result<Value, String> {
    match container {
        Value::List(xs) => {
            let i = index.as_int().ok_or("list index must be int")?;
            let idx = normalize_index(i, xs.len())?;
            let mut out = xs.clone();
            out[idx] = value;
            Ok(Value::List(out))
        }
        Value::Dict(xs) => {
            hashable(index)?;
            let mut out = xs.clone();
            if let Some(slot) = out.iter_mut().find(|(k, _)| k == index) {
                slot.1 = value;
            } else {
                out.push((index.clone(), value));
            }
            Ok(Value::Dict(out))
        }
        other => Err(format!("cannot assign into {}", type_name(other))),
    }
}

fn hashable(key: &Value) -> Result<(), String> {
    match key {
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Str(_) => Ok(()),
        other => Err(format!(
            "dict key must be None, bool, int, or str, got {}",
            type_name(other)
        )),
    }
}

pub fn dict_insert(items: &mut Vec<(Value, Value)>, key: Value, value: Value) -> Result<(), String> {
    hashable(&key)?;
    if let Some(slot) = items.iter_mut().find(|(k, _)| k == &key) {
        slot.1 = value;
    } else {
        items.push((key, value));
    }
    Ok(())
}

impl Value {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => json!({"t": "none"}),
            Value::Bool(b) => json!({"t": "bool", "v": b}),
            Value::Int(i) => json!({"t": "int", "v": i}),
            Value::Float(f) => json!({"t": "float", "v": f}),
            Value::Str(s) => json!({"t": "str", "v": s}),
            Value::List(xs) => json!({
                "t": "list",
                "v": xs.iter().map(Value::to_json).collect::<Vec<_>>()
            }),
            Value::Dict(xs) => json!({
                "t": "dict",
                "v": xs.iter().map(|(k, v)| json!([k.to_json(), v.to_json()])).collect::<Vec<_>>()
            }),
        }
    }

    pub fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        let obj = value.as_object().ok_or("value must be a tagged object")?;
        let tag = obj.get("t").and_then(|t| t.as_str()).ok_or("value is missing tag `t`")?;
        match tag {
            "none" => Ok(Value::Null),
            "bool" => Ok(Value::Bool(
                obj.get("v").and_then(|v| v.as_bool()).ok_or("bool value")?,
            )),
            "int" => {
                let n = obj.get("v").and_then(|v| v.as_i64()).ok_or("int value")?;
                Ok(Value::Int(n))
            }
            "float" => {
                let n = obj.get("v").and_then(|v| v.as_f64()).ok_or("float value")?;
                Ok(Value::Float(n))
            }
            "str" => Ok(Value::Str(
                obj.get("v")
                    .and_then(|v| v.as_str())
                    .ok_or("str value")?
                    .to_string(),
            )),
            "list" => {
                let arr = obj.get("v").and_then(|v| v.as_array()).ok_or("list value")?;
                let mut out = Vec::with_capacity(arr.len());
                for item in arr {
                    out.push(Value::from_json(item)?);
                }
                Ok(Value::List(out))
            }
            "dict" => {
                let arr = obj.get("v").and_then(|v| v.as_array()).ok_or("dict value")?;
                let mut out = Vec::with_capacity(arr.len());
                for pair in arr {
                    let pair = pair.as_array().ok_or("dict entry must be a pair")?;
                    if pair.len() != 2 {
                        return Err("dict entry must be a pair".into());
                    }
                    out.push((Value::from_json(&pair[0])?, Value::from_json(&pair[1])?));
                }
                Ok(Value::Dict(out))
            }
            other => Err(format!("unknown value tag `{other}`")),
        }
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Value::from_json(&value).map_err(serde::de::Error::custom)
    }
}



/// `format(value, spec)` for the format-spec mini-language subset Weave
/// supports: `[[fill]align][sign][0][width][,|_][.precision][type]` with type
/// one of `s d f F % e E x X o b` or none. Matches CPython on that subset.
pub fn format_value(value: &Value, spec: &str) -> Result<String, String> {
    if spec.is_empty() {
        return Ok(py_str(value));
    }
    if let Value::Bool(b) = value {
        // bool formats as int under any non-empty spec, like CPython.
        return format_value(&Value::Int(*b as i64), spec);
    }
    let bad = || format!("unsupported format spec {:?}", spec);
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let mut fill = ' ';
    let mut align: Option<char> = None;
    if chars.len() >= 2 && "<>^=".contains(chars[1]) {
        fill = chars[0];
        align = Some(chars[1]);
        i = 2;
    } else if !chars.is_empty() && "<>^=".contains(chars[0]) {
        align = Some(chars[0]);
        i = 1;
    }
    let mut sign = '-';
    if i < chars.len() && "+- ".contains(chars[i]) {
        sign = chars[i];
        i += 1;
    }
    if i < chars.len() && chars[i] == '#' {
        return Err(bad());
    }
    if i < chars.len() && chars[i] == '0' {
        if align.is_none() {
            fill = '0';
            align = Some('=');
        }
        i += 1;
    }
    let mut width = 0usize;
    while i < chars.len() && chars[i].is_ascii_digit() {
        width = width * 10 + chars[i].to_digit(10).unwrap() as usize;
        i += 1;
    }
    let mut group: Option<char> = None;
    if i < chars.len() && (chars[i] == ',' || chars[i] == '_') {
        group = Some(chars[i]);
        i += 1;
    }
    let mut precision: Option<usize> = None;
    if i < chars.len() && chars[i] == '.' {
        i += 1;
        let start = i;
        let mut p = 0usize;
        while i < chars.len() && chars[i].is_ascii_digit() {
            p = p * 10 + chars[i].to_digit(10).unwrap() as usize;
            i += 1;
        }
        if i == start {
            return Err(bad());
        }
        precision = Some(p);
    }
    let mut ty: Option<char> = None;
    if i < chars.len() {
        ty = Some(chars[i]);
        i += 1;
    }
    if i != chars.len() || width > 1000 || precision.unwrap_or(0) > 100 {
        return Err(bad());
    }
    let as_f64 = |v: &Value| -> Result<f64, String> {
        match v {
            Value::Int(n) => Ok(*n as f64),
            Value::Float(f) => Ok(*f),
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            other => Err(format!("format spec {:?} needs a number, got {}", spec, type_name(other))),
        }
    };
    let group_digits = |digits: &str, sep: char| -> String {
        let mut out = String::new();
        let n = digits.len();
        for (k, ch) in digits.chars().enumerate() {
            if k > 0 && (n - k).is_multiple_of(3) {
                out.push(sep);
            }
            out.push(ch);
        }
        out
    };
    // (sign prefix, body)
    let (neg, body, numeric) = match (ty, value) {
        (Some('s'), Value::Str(s)) | (None, Value::Str(s)) => {
            if group.is_some() || sign != '-' || align == Some('=') {
                return Err(bad());
            }
            let s: String = match precision {
                Some(p) => s.chars().take(p).collect(),
                None => s.clone(),
            };
            (false, s, false)
        }
        (Some('s'), _) => return Err(format!("format spec {:?} needs a str, got {}", spec, type_name(value))),
        (Some('d' | 'x' | 'X' | 'o' | 'b'), Value::Int(_) | Value::Bool(_)) | (None, Value::Int(_)) if precision.is_none() => {
            let n = match value {
                Value::Int(n) => *n,
                Value::Bool(b) => *b as i64,
                _ => unreachable!(),
            };
            let mag = n.unsigned_abs();
            let digits = match ty {
                Some('x') => format!("{mag:x}"),
                Some('X') => format!("{mag:X}"),
                Some('o') => format!("{mag:o}"),
                Some('b') => format!("{mag:b}"),
                _ => mag.to_string(),
            };
            let digits = match group {
                Some(sep) => group_digits(&digits, sep),
                None => digits,
            };
            (n < 0, digits, true)
        }
        (Some('d' | 'x' | 'X' | 'o' | 'b'), other) => {
            return Err(format!("format spec {:?} needs an int, got {}", spec, type_name(other)))
        }
        (Some('f' | 'F' | '%' | 'e' | 'E'), _) | (None, Value::Float(_)) => {
            let mut f = as_f64(value)?;
            let t = ty.unwrap_or('g');
            if t == 'g' {
                // No type on a float: Python's repr, or `.Ng` when a precision is given.
                if precision.is_some() {
                    return Err(format!("format spec {:?}: use an explicit type such as `.2f`", spec));
                }
                (f.is_sign_negative(), py_str(&Value::Float(f.abs())), true)
            } else {
                if t == '%' {
                    f *= 100.0;
                }
                let p = precision.unwrap_or(6);
                let neg = f < 0.0 || (f == 0.0 && f.is_sign_negative());
                let a = f.abs();
                let mut text = if a.is_nan() {
                    "nan".to_string()
                } else if a.is_infinite() {
                    "inf".to_string()
                } else if t == 'e' || t == 'E' {
                    let raw = format!("{a:.p$e}");
                    let (mant, exp) = raw.split_once('e').unwrap_or((&raw, "0"));
                    let e: i32 = exp.parse().unwrap_or(0);
                    format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
                } else {
                    format!("{a:.p$}")
                };
                if let Some(sep) = group {
                    let (int_part, frac) = match text.find('.') {
                        Some(k) => (text[..k].to_string(), text[k..].to_string()),
                        None => (text.clone(), String::new()),
                    };
                    if int_part.chars().all(|c| c.is_ascii_digit()) {
                        text = format!("{}{frac}", group_digits(&int_part, sep));
                    }
                }
                if t == 'F' || t == 'E' {
                    text = text.to_uppercase();
                }
                if t == '%' {
                    text.push('%');
                }
                (neg, text, true)
            }
        }
        _ => return Err(format!("format spec {:?} is not supported for {}", spec, type_name(value))),
    };
    let sign_text = if neg {
        "-"
    } else if numeric && sign == '+' {
        "+"
    } else if numeric && sign == ' ' {
        " "
    } else {
        ""
    };
    let len = sign_text.chars().count() + body.chars().count();
    if width <= len {
        return Ok(format!("{sign_text}{body}"));
    }
    let pad = width - len;
    let fill_s = |n: usize| std::iter::repeat_n(fill, n).collect::<String>();
    let align = align.unwrap_or(if numeric { '>' } else { '<' });
    Ok(match align {
        '<' => format!("{sign_text}{body}{}", fill_s(pad)),
        '>' => format!("{}{sign_text}{body}", fill_s(pad)),
        '^' => format!("{}{sign_text}{body}{}", fill_s(pad / 2), fill_s(pad - pad / 2)),
        _ => format!("{sign_text}{}{body}", fill_s(pad)),
    })
}

/// `"...".format(*args)` with `{}`, `{0}`, `{!r}` and `{:spec}` fields.
pub fn str_format(template: &str, args: &[Value]) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    let mut auto = 0usize;
    while let Some(ch) = chars.next() {
        match ch {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '}' => return Err("format(): single '}' in the template".into()),
            '{' => {
                let mut field = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') => return Err("format(): nested fields are not supported".into()),
                        Some(c) => field.push(c),
                        None => return Err("format(): unclosed '{' in the template".into()),
                    }
                }
                let (head, spec) = match field.split_once(':') {
                    Some((h, s)) => (h.to_string(), s.to_string()),
                    None => (field.clone(), String::new()),
                };
                let (name, conv) = match head.split_once('!') {
                    Some((n, c)) => (n.to_string(), Some(c.to_string())),
                    None => (head, None),
                };
                let idx = if name.is_empty() {
                    auto += 1;
                    auto - 1
                } else {
                    name.parse::<usize>().map_err(|_| {
                        format!("format(): named field {{{name}}} is not supported; use an f-string")
                    })?
                };
                let v = args
                    .get(idx)
                    .ok_or_else(|| format!("format(): field {idx} has no argument (got {})", args.len()))?;
                let v = match conv.as_deref() {
                    None => v.clone(),
                    Some("s") => Value::Str(py_str(v)),
                    Some("r") => Value::Str(py_repr(v)),
                    Some(c) => return Err(format!("format(): unknown conversion !{c}")),
                };
                out.push_str(&format_value(&v, &spec)?);
            }
            c => out.push(c),
        }
    }
    Ok(out)
}
