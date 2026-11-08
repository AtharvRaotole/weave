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

fn as_number(value: &Value) -> Result<Num, String> {
    match value {
        Value::Bool(b) => Ok(Num::Int(if *b { 1 } else { 0 })),
        Value::Int(i) => Ok(Num::Int(*i)),
        Value::Float(f) if f.is_finite() => Ok(Num::Float(*f)),
        Value::Float(_) => Err("float is not finite".into()),
        other => Err(format!(
            "expected a number, got {}",
            type_name(other)
        )),
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
        (Value::Str(s), Value::Int(n)) | (Value::Int(n), Value::Str(s)) => Ok(Value::Str(repeat_str(s, *n))),
        (Value::List(xs), Value::Int(n)) | (Value::Int(n), Value::List(xs)) => {
            Ok(Value::List(repeat_list(xs, *n)))
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

fn repeat_str(s: &str, n: i64) -> String {
    if n <= 0 {
        return String::new();
    }
    s.repeat(n as usize)
}

fn repeat_list(xs: &[Value], n: i64) -> Vec<Value> {
    if n <= 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(xs.len() * n as usize);
    for _ in 0..n {
        out.extend(xs.iter().cloned());
    }
    out
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
            Ok(Value::Int(x.div_euclid(y)))
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
                Ok(Value::Int(x.rem_euclid(y)))
            }
            (x, y) => {
                let y = to_f(y);
                if y == 0.0 {
                    return Err("division by zero".into());
                }
                Ok(Value::Float(to_f(x).rem_euclid(y)))
            }
        },
    }
}

fn format_percent(fmt: &str, value: &Value) -> Result<String, String> {
    // Only `%s` is supported, matching the common f-string alternative agents write.
    if fmt == "%s" {
        return Ok(py_str(value));
    }
    Err("only `%s` string formatting is supported; use an f-string".into())
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
    let ord = match (left, right) {
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::Str(a), Value::Str(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        _ => {
            if let (Ok(a), Ok(b)) = (as_number(left), as_number(right)) {
                let af = to_f(a);
                let bf = to_f(b);
                af.partial_cmp(&bf)
                    .ok_or_else(|| "cannot compare these numbers".to_string())?
            } else {
                return Err(format!(
                    "cannot compare {} and {} with `{}`",
                    type_name(left),
                    type_name(right),
                    cmp_token(op)
                ));
            }
        }
    };
    Ok(match op {
        Cmp::Lt => ord.is_lt(),
        Cmp::LtE => ord.is_le(),
        Cmp::Gt => ord.is_gt(),
        Cmp::GtE => ord.is_ge(),
        _ => unreachable!(),
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
            let i = index
                .as_int()
                .ok_or("list index must be int")?;
            let idx = normalize_index(i, xs.len())?;
            Ok(xs[idx].clone())
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let i = index.as_int().ok_or("string index must be int")?;
            let idx = normalize_index(i, chars.len())?;
            Ok(Value::Str(chars[idx].to_string()))
        }
        Value::Dict(xs) => dict_get(xs, index)
            .cloned()
            .ok_or_else(|| format!("key {} is missing", py_repr(index))),
        other => Err(format!("cannot index {}", type_name(other))),
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

