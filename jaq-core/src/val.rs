//! JSON values with reference-counted sharing.

use crate::{Error, Int};
use ahash::RandomState;
use alloc::string::{String, ToString};
use alloc::{boxed::Box, rc::Rc, vec::Vec};
use core::cmp::Ordering;
use core::fmt;
use indexmap::IndexMap;
use jaq_parse::MathOp;

/// JSON value with sharing.
///
/// The speciality of this type is that numbers are distinguished into
/// positive integers, negative integers, and 64-bit floating-point numbers.
/// All integers are machine-sized.
/// This allows using integers to index arrays,
/// while using floating-point numbers to do general math.
///
/// Operations on numbers follow a few principles:
/// * The sum, difference, product, and remainder of two integers is integer.
/// * The quotient of two integers is integer if the remainder of the integers is zero.
/// * Any other operation between two numbers yields a float.
#[derive(Clone, Debug)]
pub enum Val {
    Null,
    /// Boolean
    Bool(bool),
    /// Integer
    Int(Int),
    /// Floating-point number
    Float(f64),
    /// Floating-point number or integer not fitting into `Int`
    Num(Rc<serde_json::Number>),
    /// String
    Str(Rc<String>),
    /// Array
    Arr(Rc<Vec<Val>>),
    /// Order-preserving map
    Obj(Rc<IndexMap<Rc<String>, Val, RandomState>>),
}

/// A value result.
pub type ValR = Result<Val, Error>;

/// A stream of values.
type Vals<'a> = Box<dyn Iterator<Item = Val> + 'a>;

/// A stream of value results.
pub type ValRs<'a> = Box<dyn Iterator<Item = ValR> + 'a>;

impl Val {
    /// True if the value is neither null nor false.
    pub fn as_bool(&self) -> bool {
        !matches!(self, Val::Null | Val::Bool(false))
    }

    /// If the value is a positive integer, return it, else fail.
    pub fn as_usize(&self) -> Result<usize, Error> {
        match self {
            Self::Int(i) if i.is_positive() => Ok(i.abs()),
            _ => Err(Error::Nat(self.clone())),
        }
    }

    /// If the value is integer, return its absolute value and whether its positive, else fail.
    pub fn as_int(&self) -> Result<Int, Error> {
        match self {
            Self::Int(i) => Ok(*i),
            _ => Err(Error::Int(self.clone())),
        }
    }

    /// If the value is a string, return it, else fail.
    pub fn as_obj_key(&self) -> Result<Rc<String>, Error> {
        match self {
            Self::Str(s) => Ok(Rc::clone(s)),
            _ => Err(Error::ObjKey(self.clone())),
        }
    }

    /// Return 0 for null, the absolute value for numbers, and
    /// the length for strings, arrays, and objects.
    ///
    /// Fail on booleans.
    pub fn len(&self) -> Result<Self, Error> {
        match self {
            Self::Null => Ok(Self::Int(Int::from(0))),
            Self::Bool(_) => Err(Error::Length(self.clone())),
            Self::Int(i) => Ok(Self::Int(Int::from(i.abs()))),
            Self::Num(n) => Self::from(&**n).len(),
            Self::Float(f) => Ok(Self::Float(f.abs())),
            Self::Str(s) => Ok(Self::Int(s.chars().count().into())),
            Self::Arr(a) => Ok(Self::Int(a.len().into())),
            Self::Obj(o) => Ok(Self::Int(o.len().into())),
        }
    }

    /// Return the type of the value, e.g. "boolean" for true and false.
    pub fn typ(&self) -> &str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Int(_) | Self::Float(_) | Self::Num(_) => "number",
            Self::Str(_) => "string",
            Self::Arr(_) => "array",
            Self::Obj(_) => "object",
        }
    }

    /// Apply a rounding function to floating-point numbers, then convert them to integers.
    ///
    /// Return integers unchanged, and fail on any other input.
    pub fn round(&self, f: impl FnOnce(f64) -> f64) -> Result<Self, Error> {
        match self {
            Self::Int(_) => Ok(self.clone()),
            Self::Float(x) => {
                let rounded = f(*x);
                if rounded < 0.0 {
                    Ok(Self::Int(-Int::from(-rounded as usize)))
                } else {
                    Ok(Self::Int(Int::from(rounded as usize)))
                }
            }
            Self::Num(n) => Self::from(&**n).round(f),
            _ => Err(Error::Round(self.clone())),
        }
    }

    /// Return all numbers in the range `[self, other)` if both inputs are integer, else fail.
    pub fn range(&self, other: &Self) -> Result<Box<dyn Iterator<Item = Self>>, Error> {
        match (self, other) {
            (Self::Int(x), Self::Int(y)) => Ok(Box::new(x.range(y).map(Self::Int))),
            _ => Err(Error::Range),
        }
    }

    /// Return true if `value | .[key]` is defined.
    ///
    /// Fail on values that are neither null, arrays, nor objects.
    pub fn has(&self, key: &Self) -> Result<bool, Error> {
        match (self, key) {
            (Self::Null, _) => Ok(false),
            (Self::Arr(a), Self::Int(i)) if i.is_positive() => Ok(i.abs() < a.len()),
            (Self::Obj(o), Self::Str(s)) => Ok(o.contains_key(&**s)),
            _ => Err(Error::Has(self.clone(), key.clone())),
        }
    }

    /// Return any `key` for which `value | .[key]` is defined.
    ///
    /// Fail on values that are neither arrays nor objects.
    pub fn keys(&self) -> Result<Vals, Error> {
        match self {
            Self::Arr(a) => Ok(Box::new((0..a.len()).map(|i| Val::Int(i.into())))),
            Self::Obj(o) => Ok(Box::new(o.keys().map(|k| Val::Str(Rc::clone(k))))),
            _ => Err(Error::Keys(self.clone())),
        }
    }

    /// Return the elements of an array or the values of an object (omitting its keys).
    ///
    /// Fail on any other value.
    pub fn iter(&self) -> Result<Vals, Error> {
        match self {
            Self::Arr(a) => Ok(Box::new(a.iter().cloned())),
            Self::Obj(o) => Ok(Box::new(o.values().cloned())),
            _ => Err(Error::Iter(self.clone())),
        }
    }

    /// `a` contains `b` iff either
    /// * the string `b` is a substring of `a`,
    /// * every element in the array `b` is contained in some element of the array `a`,
    /// * for every key-value pair `k, v` in `b`,
    ///   there is a key-value pair `k, v'` in `a` such that `v'` contains `v`, or
    /// * `a equals `b`.
    pub fn contains(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Str(l), Self::Str(r)) => l.contains(&**r),
            (Self::Arr(l), Self::Arr(r)) => r.iter().all(|r| l.iter().any(|l| l.contains(r))),
            (Self::Obj(l), Self::Obj(r)) => r
                .iter()
                .all(|(k, r)| l.get(k).map(|l| l.contains(r)).unwrap_or(false)),
            _ => self == other,
        }
    }

    /// Convert string to JSON.
    ///
    /// Fail on any other value.
    pub fn from_json(self) -> ValR {
        match self {
            Self::Str(ref s) => serde_json::from_str::<serde_json::Value>(s)
                .map(Val::from)
                .map_err(|e| Error::FromJson(self, Some(e.to_string()))),
            _ => Err(Error::FromJson(self, None)),
        }
    }

    /// Sort array by the default order.
    ///
    /// Fail on any other value.
    pub fn sort(self) -> ValR {
        match self {
            Val::Arr(mut a) => {
                Rc::make_mut(&mut a).sort();
                Ok(Val::Arr(a))
            }
            v => Err(Error::Sort(v)),
        }
    }

    /// Sort array by the given function.
    ///
    /// Fail on any other value.
    pub fn sort_by<'a>(self, f: impl Fn(Val) -> ValRs<'a>) -> ValR {
        match self {
            Self::Arr(mut a) => {
                // Some(e) iff an error has previously occurred
                let mut err = None;
                Rc::make_mut(&mut a).sort_by_cached_key(|x| {
                    if err.is_some() {
                        return Vec::new();
                    };
                    match f(x.clone()).collect() {
                        Ok(y) => y,
                        Err(e) => {
                            err = Some(e);
                            Vec::new()
                        }
                    }
                });
                err.map_or(Ok(Val::Arr(a)), Err)
            }
            _ => Err(Error::Sort(self)),
        }
    }
}

impl From<serde_json::Value> for Val {
    fn from(v: serde_json::Value) -> Self {
        use serde_json::Value::*;
        match v {
            Null => Self::Null,
            Bool(b) => Self::Bool(b),
            Number(n) => {
                // try to map numbers into positive/negative integers
                let s = n.to_string();
                let n = || Self::Num(Rc::new(n));
                match s.strip_prefix('-') {
                    Some(neg) => neg
                        .parse::<usize>()
                        .map_or_else(|_| n(), |u| Self::Int(-Int::from(u))),
                    None => s
                        .parse::<usize>()
                        .map_or_else(|_| n(), |p| Self::Int(Int::from(p))),
                }
            }
            String(s) => Self::Str(Rc::new(s)),
            Array(a) => Self::Arr(Rc::new(a.into_iter().map(|x| x.into()).collect())),
            Object(o) => Self::Obj(Rc::new(
                o.into_iter().map(|(k, v)| (Rc::new(k), v.into())).collect(),
            )),
        }
    }
}

impl From<Val> for serde_json::Value {
    fn from(v: Val) -> serde_json::Value {
        use serde_json::Value::*;
        match v {
            Val::Null => Null,
            Val::Bool(b) => Bool(b),
            Val::Int(i) if i.is_positive() => Number(i.abs().into()),
            Val::Int(i) => isize::try_from(i.abs()).map_or(Null, |n| Number((-n).into())),
            Val::Float(f) => serde_json::Number::from_f64(f).map_or(Null, Number),
            Val::Num(n) => Number((*n).clone()),
            Val::Str(s) => String((*s).clone()),
            Val::Arr(a) => Array(a.iter().map(|x| x.clone().into()).collect()),
            Val::Obj(o) => Object(
                o.iter()
                    .map(|(k, v)| ((**k).clone(), v.clone().into()))
                    .collect(),
            ),
        }
    }
}

impl From<&serde_json::Number> for Val {
    fn from(n: &serde_json::Number) -> Self {
        n.to_string().parse().map_or(Self::Null, Self::Float)
    }
}

impl core::ops::Add for Val {
    type Output = ValR;
    fn add(self, rhs: Self) -> Self::Output {
        use Val::*;
        match (self, rhs) {
            // `null` is a neutral element for addition
            (Null, x) | (x, Null) => Ok(x),
            (Int(x), Int(y)) => Ok(Int(x + y)),
            (Int(i), Float(f)) | (Float(f), Int(i)) => Ok(Float(f + i.as_f64())),
            (Float(x), Float(y)) => Ok(Float(x + y)),
            (Num(n), r) => Self::from(&*n) + r,
            (l, Num(n)) => l + Self::from(&*n),
            (Str(mut l), Str(r)) => {
                Rc::make_mut(&mut l).push_str(&r);
                Ok(Str(l))
            }
            (Arr(mut l), Arr(r)) => {
                Rc::make_mut(&mut l).extend(r.iter().cloned());
                Ok(Arr(l))
            }
            (Obj(mut l), Obj(r)) => {
                Rc::make_mut(&mut l).extend(r.iter().map(|(k, v)| (k.clone(), v.clone())));
                Ok(Obj(l))
            }
            (l, r) => Err(Error::MathOp(l, MathOp::Add, r)),
        }
    }
}

impl core::ops::Sub for Val {
    type Output = ValR;
    fn sub(self, rhs: Self) -> Self::Output {
        use Val::*;
        match (self, rhs) {
            (Int(x), Int(y)) => Ok(Int(x - y)),
            (Float(f), Int(i)) => Ok(Float(f - i.as_f64())),
            (Int(i), Float(f)) => Ok(Float(i.as_f64() - f)),
            (Float(x), Float(y)) => Ok(Float(x - y)),
            (Num(n), r) => Self::from(&*n) - r,
            (l, Num(n)) => l - Self::from(&*n),
            (l, r) => Err(Error::MathOp(l, MathOp::Sub, r)),
        }
    }
}

impl core::ops::Mul for Val {
    type Output = ValR;
    fn mul(self, rhs: Self) -> Self::Output {
        use Val::*;
        match (self, rhs) {
            (Int(x), Int(y)) => Ok(Int(x * y)),
            (Float(f), Int(i)) | (Int(i), Float(f)) => Ok(Float(f * i.as_f64())),
            (Float(x), Float(y)) => Ok(Float(x * y)),
            (Num(n), r) => Self::from(&*n) * r,
            (l, Num(n)) => l * Self::from(&*n),
            (l, r) => Err(Error::MathOp(l, MathOp::Mul, r)),
        }
    }
}

impl core::ops::Div for Val {
    type Output = ValR;
    fn div(self, rhs: Self) -> Self::Output {
        use Val::*;
        match (self, rhs) {
            (Int(x), Int(y)) => Ok(Int(x / y)),
            (Float(f), Int(i)) => Ok(Float(f / i.as_f64())),
            (Int(i), Float(f)) => Ok(Float(i.as_f64() / f)),
            (Float(x), Float(y)) => Ok(Float(x / y)),
            (Num(n), r) => Self::from(&*n) / r,
            (l, Num(n)) => l / Self::from(&*n),
            (l, r) => Err(Error::MathOp(l, MathOp::Div, r)),
        }
    }
}

impl core::ops::Rem for Val {
    type Output = ValR;
    fn rem(self, rhs: Self) -> Self::Output {
        use Val::*;
        match (self, rhs) {
            (Int(x), Int(y)) => Ok(Int(x % y)),
            (l, r) => Err(Error::MathOp(l, MathOp::Rem, r)),
        }
    }
}

impl core::ops::Neg for Val {
    type Output = ValR;
    fn neg(self) -> Self::Output {
        use Val::*;
        match self {
            Int(x) => Ok(Int(-x)),
            Float(x) => Ok(Float(-x)),
            Num(n) => -Self::from(&*n),
            x => Err(Error::Neg(x)),
        }
    }
}

impl PartialEq for Val {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(x), Self::Bool(y)) => x == y,
            (Self::Int(x), Self::Int(y)) => x == y,
            (Self::Int(i), Self::Float(f)) | (Self::Float(f), Self::Int(i)) => {
                float_eq(&i.as_f64(), f)
            }
            (Self::Float(x), Self::Float(y)) => float_eq(x, y),
            (Self::Num(x), Self::Num(y)) if Rc::ptr_eq(x, y) => true,
            (Self::Num(n), y) => &Self::from(&**n) == y,
            (x, Self::Num(n)) => x == &Self::from(&**n),
            (Self::Str(x), Self::Str(y)) => x == y,
            (Self::Arr(x), Self::Arr(y)) => x == y,
            (Self::Obj(x), Self::Obj(y)) => x == y,
            _ => false,
        }
    }
}

impl Eq for Val {}

impl PartialOrd for Val {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Val {
    fn cmp(&self, other: &Self) -> Ordering {
        use Ordering::*;
        match (self, other) {
            (Self::Null, Self::Null) => Equal,
            (Self::Bool(x), Self::Bool(y)) => x.cmp(y),
            (Self::Int(x), Self::Int(y)) => x.cmp(y),
            (Self::Int(i), Self::Float(f)) => float_cmp(&i.as_f64(), f),
            (Self::Float(f), Self::Int(i)) => float_cmp(f, &i.as_f64()),
            (Self::Float(x), Self::Float(y)) => float_cmp(x, y),
            (Self::Num(x), Self::Num(y)) if Rc::ptr_eq(x, y) => Equal,
            (Self::Num(n), y) => Self::from(&**n).cmp(y),
            (x, Self::Num(n)) => x.cmp(&Self::from(&**n)),
            (Self::Str(x), Self::Str(y)) => x.cmp(y),
            (Self::Arr(x), Self::Arr(y)) => x.cmp(y),
            (Self::Obj(x), Self::Obj(y)) => {
                let mut l: Vec<_> = x.iter().collect();
                let mut r: Vec<_> = y.iter().collect();
                l.sort_by_key(|(k, _v)| *k);
                r.sort_by_key(|(k, _v)| *k);
                // TODO: make this nicer
                let kl = l.iter().map(|(k, _v)| k);
                let kr = r.iter().map(|(k, _v)| k);
                let vl = l.iter().map(|(_k, v)| v);
                let vr = r.iter().map(|(_k, v)| v);
                kl.cmp(kr).then_with(|| vl.cmp(vr))
            }

            // nulls are smaller than anything else
            (Self::Null, _) => Less,
            (_, Self::Null) => Greater,
            // bools are smaller than anything else, except for nulls
            (Self::Bool(_), _) => Less,
            (_, Self::Bool(_)) => Greater,
            // numbers are smaller than anything else, except for nulls and bools
            (Self::Int(_) | Self::Float(_), _) => Less,
            (_, Self::Int(_) | Self::Float(_)) => Greater,
            // etc.
            (Self::Str(_), _) => Less,
            (_, Self::Str(_)) => Greater,
            (Self::Arr(_), _) => Less,
            (_, Self::Arr(_)) => Greater,
        }
    }
}

fn float_eq(left: &f64, right: &f64) -> bool {
    float_cmp(left, right) == Ordering::Equal
}

// adapted from the currently nightly-only function `f64::total_cmp`
fn float_cmp(left: &f64, right: &f64) -> Ordering {
    if *left == 0. && *right == 0. {
        return Ordering::Equal;
    }

    let mut left = left.to_bits() as i64;
    let mut right = right.to_bits() as i64;

    left ^= (((left >> 63) as u64) >> 1) as i64;
    right ^= (((right >> 63) as u64) >> 1) as i64;

    left.cmp(&right)
}

impl fmt::Display for Val {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(b) => b.fmt(f),
            Self::Int(i) => i.fmt(f),
            Self::Float(x) => x.fmt(f),
            Self::Num(n) => n.fmt(f),
            Self::Str(s) => write!(f, "\"{}\"", s),
            Self::Arr(a) => {
                write!(f, "[")?;
                let mut iter = a.iter();
                if let Some(first) = iter.next() {
                    first.fmt(f)?
                };
                iter.try_for_each(|x| write!(f, ",{}", x))?;
                write!(f, "]")
            }
            Self::Obj(o) => {
                write!(f, "{{")?;
                let mut iter = o.iter();
                if let Some((k, v)) = iter.next() {
                    write!(f, "\"{}\":{}", k, v)?;
                }
                iter.try_for_each(|(k, v)| write!(f, ",\"{}\":{}", k, v))?;
                write!(f, "}}")
            }
        }
    }
}
