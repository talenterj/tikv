// Copyright 2019 TiKV Project Authors. Licensed under Apache-2.0.

mod bit_vec;
mod chunked_vec_bytes;
mod chunked_vec_common;
mod chunked_vec_json;
mod chunked_vec_sized;
mod logical_rows;
mod scalar;
mod vector;
pub use logical_rows::{LogicalRows, BATCH_MAX_SIZE, IDENTICAL_LOGICAL_ROWS};

// Concrete eval types without a nullable wrapper.
pub type Int = i64;
pub type Real = ordered_float::NotNan<f64>;
pub type Bytes = Vec<u8>;
pub type BytesRef<'a> = &'a [u8];
pub use crate::codec::mysql::{json::JsonRef, Decimal, Duration, Json, JsonType, Time as DateTime};
pub use bit_vec::{BitAndIterator, BitVec};
pub use chunked_vec_bytes::{BytesGuard, BytesWriter, ChunkedVecBytes, PartialBytesWriter};
pub use chunked_vec_json::ChunkedVecJson;
pub use chunked_vec_sized::ChunkedVecSized;

// Dynamic eval types.
pub use self::scalar::{ScalarValue, ScalarValueRef};
pub use self::vector::{VectorValue, VectorValueExt};

use crate::EvalType;

use crate::codec::convert::ConvertTo;
use crate::expr::EvalContext;
use tidb_query_common::error::Result;

/// A trait of evaluating current concrete eval type into a MySQL logic value, represented by
/// Rust's `bool` type.
pub trait AsMySQLBool {
    /// Evaluates into a MySQL logic value.
    fn as_mysql_bool(&self, context: &mut EvalContext) -> Result<bool>;
}

impl AsMySQLBool for Int {
    #[inline]
    fn as_mysql_bool(&self, _context: &mut EvalContext) -> Result<bool> {
        Ok(*self != 0)
    }
}

impl AsMySQLBool for Real {
    #[inline]
    fn as_mysql_bool(&self, _context: &mut EvalContext) -> Result<bool> {
        Ok(self.into_inner() != 0f64)
    }
}

impl<'a, T: AsMySQLBool> AsMySQLBool for &'a T {
    #[inline]
    fn as_mysql_bool(&self, context: &mut EvalContext) -> Result<bool> {
        (&**self).as_mysql_bool(context)
    }
}

impl AsMySQLBool for Bytes {
    #[inline]
    fn as_mysql_bool(&self, context: &mut EvalContext) -> Result<bool> {
        self.as_slice().as_mysql_bool(context)
    }
}

impl<'a> AsMySQLBool for BytesRef<'a> {
    #[inline]
    fn as_mysql_bool(&self, context: &mut EvalContext) -> Result<bool> {
        Ok(!self.is_empty() && ConvertTo::<f64>::convert(self, context)? != 0f64)
    }
}

impl<'a, T> AsMySQLBool for Option<&'a T>
where
    T: AsMySQLBool,
{
    fn as_mysql_bool(&self, context: &mut EvalContext) -> Result<bool> {
        match self {
            None => Ok(false),
            Some(ref v) => v.as_mysql_bool(context),
        }
    }
}

impl<'a> AsMySQLBool for JsonRef<'a> {
    fn as_mysql_bool(&self, _context: &mut EvalContext) -> Result<bool> {
        // TODO: This logic is not correct. See pingcap/tidb#9593
        Ok(false)
    }
}

impl<'a> AsMySQLBool for Option<BytesRef<'a>> {
    fn as_mysql_bool(&self, context: &mut EvalContext) -> Result<bool> {
        match self {
            None => Ok(false),
            Some(ref v) => v.as_mysql_bool(context),
        }
    }
}

impl<'a> AsMySQLBool for Option<JsonRef<'a>> {
    fn as_mysql_bool(&self, context: &mut EvalContext) -> Result<bool> {
        match self {
            None => Ok(false),
            Some(ref v) => v.as_mysql_bool(context),
        }
    }
}

pub macro match_template_evaluable($t:tt, $($tail:tt)*) {
    match_template::match_template! {
        $t = [Int, Real, Decimal, Bytes, DateTime, Duration, Json],
        $($tail)*
    }
}

pub trait ChunkRef<'a, T: EvaluableRef<'a>>: Copy + Clone + std::fmt::Debug + Send + Sync {
    fn get_option_ref(self, idx: usize) -> Option<T>;

    fn get_bit_vec(self) -> &'a BitVec;

    fn phantom_data(self) -> Option<T>;
}

pub trait UnsafeRefInto<T> {
    /// # Safety
    ///
    /// This function uses `std::mem::transmute`.
    /// The only place that copr uses this function is in
    /// `tidb_query_aggr`, together with a set of `update` macros.
    unsafe fn unsafe_into(self) -> T;
}

/// A trait of all types that can be used during evaluation (eval type).
pub trait Evaluable: Clone + std::fmt::Debug + Send + Sync + 'static {
    const EVAL_TYPE: EvalType;

    /// Borrows this concrete type from a `ScalarValue` in the same type;
    /// panics if the varient mismatches.
    fn borrow_scalar_value(v: &ScalarValue) -> Option<&Self>;

    /// Borrows this concrete type from a `ScalarValueRef` in the same type;
    /// panics if the varient mismatches.
    fn borrow_scalar_value_ref(v: ScalarValueRef<'_>) -> Option<&Self>;

    /// Borrows a slice of this concrete type from a `VectorValue` in the same type;
    /// panics if the varient mismatches.
    fn borrow_vector_value(v: &VectorValue) -> &ChunkedVecSized<Self>;
}

pub trait EvaluableRet: Clone + std::fmt::Debug + Send + Sync + 'static {
    const EVAL_TYPE: EvalType;
    type ChunkedType: ChunkedVec<Self>;
    /// Converts a vector of this concrete type into a `VectorValue` in the same type;
    /// panics if the varient mismatches.
    fn into_vector_value(vec: Self::ChunkedType) -> VectorValue;
}

/// # Notes
///
/// Make sure operating `bitmap` and `value` together, so while `bitmap` is 0 and the
/// corresponding value is None.
///
/// With this guaranty, we can avoid the following issue:
///
/// For Data [Some(1), Some(2), None], we could have different stored representation:
///
/// Bitmap: 110, Value: 1, 2, 0
/// Bitmap: 110, Value: 1, 2, 1
/// Bitmap: 110, Value: 1, 2, 3
///
/// `PartialEq` between `Value`'s result could be wrong.
pub trait ChunkedVec<T> {
    fn chunked_with_capacity(capacity: usize) -> Self;
    fn chunked_push(&mut self, value: Option<T>);
}

macro_rules! impl_evaluable_type {
    ($ty:tt) => {
        impl Evaluable for $ty {
            const EVAL_TYPE: EvalType = EvalType::$ty;

            #[inline]
            fn borrow_scalar_value(v: &ScalarValue) -> Option<&Self> {
                match v {
                    ScalarValue::$ty(x) => x.as_ref(),
                    _ => unimplemented!(),
                }
            }

            #[inline]
            fn borrow_scalar_value_ref<'a>(v: ScalarValueRef<'a>) -> Option<&'a Self> {
                match v {
                    ScalarValueRef::$ty(x) => x,
                    _ => unimplemented!(),
                }
            }

            #[inline]
            fn borrow_vector_value(v: &VectorValue) -> &ChunkedVecSized<$ty> {
                match v {
                    VectorValue::$ty(x) => x,
                    _ => unimplemented!(),
                }
            }
        }
    };
}

impl_evaluable_type! { Int }
impl_evaluable_type! { Real }
impl_evaluable_type! { Decimal }
impl_evaluable_type! { DateTime }
impl_evaluable_type! { Duration }

macro_rules! impl_evaluable_ret {
    ($ty:tt, $chunk:ty) => {
        impl EvaluableRet for $ty {
            const EVAL_TYPE: EvalType = EvalType::$ty;
            type ChunkedType = $chunk;

            #[inline]
            fn into_vector_value(vec: $chunk) -> VectorValue {
                VectorValue::from(vec)
            }
        }
    };
}

impl_evaluable_ret! { Int, ChunkedVecSized<Self> }
impl_evaluable_ret! { Real, ChunkedVecSized<Self> }
impl_evaluable_ret! { Decimal, ChunkedVecSized<Self> }
impl_evaluable_ret! { Bytes, ChunkedVecBytes }
impl_evaluable_ret! { DateTime, ChunkedVecSized<Self> }
impl_evaluable_ret! { Duration, ChunkedVecSized<Self> }
impl_evaluable_ret! { Json, ChunkedVecJson }

pub trait EvaluableRef<'a>: Clone + std::fmt::Debug + Send + Sync {
    const EVAL_TYPE: EvalType;
    type ChunkedType: ChunkRef<'a, Self> + 'a;
    type EvaluableType: EvaluableRet;

    /// Borrows this concrete type from a `ScalarValue` in the same type;
    /// panics if the varient mismatches.
    fn borrow_scalar_value(v: &'a ScalarValue) -> Option<Self>;

    /// Borrows this concrete type from a `ScalarValueRef` in the same type;
    /// panics if the varient mismatches.
    fn borrow_scalar_value_ref(v: ScalarValueRef<'a>) -> Option<Self>;

    /// Borrows a slice of this concrete type from a `VectorValue` in the same type;
    /// panics if the varient mismatches.
    fn borrow_vector_value(v: &'a VectorValue) -> Self::ChunkedType;

    /// Convert this reference to owned type
    fn to_owned_value(self) -> Self::EvaluableType;

    fn from_owned_value(value: &'a Self::EvaluableType) -> Self;
}

impl<'a, T: Evaluable + EvaluableRet> EvaluableRef<'a> for &'a T {
    const EVAL_TYPE: EvalType = <T as Evaluable>::EVAL_TYPE;
    type ChunkedType = &'a ChunkedVecSized<T>;
    type EvaluableType = T;

    #[inline]
    fn borrow_scalar_value(v: &'a ScalarValue) -> Option<Self> {
        Evaluable::borrow_scalar_value(v)
    }

    #[inline]
    fn borrow_scalar_value_ref(v: ScalarValueRef<'a>) -> Option<Self> {
        Evaluable::borrow_scalar_value_ref(v)
    }

    #[inline]
    fn borrow_vector_value(v: &'a VectorValue) -> &'a ChunkedVecSized<T> {
        Evaluable::borrow_vector_value(v)
    }

    #[inline]
    fn to_owned_value(self) -> Self::EvaluableType {
        self.clone()
    }

    #[inline]
    fn from_owned_value(value: &'a T) -> Self {
        &value
    }
}

impl<'a, A: UnsafeRefInto<B>, B> UnsafeRefInto<Option<B>> for Option<A> {
    unsafe fn unsafe_into(self) -> Option<B> {
        self.map(|x| x.unsafe_into())
    }
}

impl<'a, T: Evaluable + EvaluableRet> UnsafeRefInto<&'static T> for &'a T {
    unsafe fn unsafe_into(self) -> &'static T {
        std::mem::transmute(self)
    }
}

impl<'a> EvaluableRef<'a> for BytesRef<'a> {
    const EVAL_TYPE: EvalType = EvalType::Bytes;
    type EvaluableType = Bytes;
    type ChunkedType = &'a ChunkedVecBytes;

    #[inline]
    fn borrow_scalar_value(v: &'a ScalarValue) -> Option<Self> {
        match v {
            ScalarValue::Bytes(x) => x.as_ref().map(|x| x.as_slice()),
            _ => unimplemented!(),
        }
    }

    #[inline]
    fn borrow_scalar_value_ref(v: ScalarValueRef<'a>) -> Option<Self> {
        match v {
            ScalarValueRef::Bytes(x) => x,
            _ => unimplemented!(),
        }
    }

    #[inline]
    fn borrow_vector_value(v: &'a VectorValue) -> &'a ChunkedVecBytes {
        match v {
            VectorValue::Bytes(x) => x,
            _ => unimplemented!(),
        }
    }

    #[inline]
    fn to_owned_value(self) -> Self::EvaluableType {
        self.to_vec()
    }

    #[inline]
    fn from_owned_value(value: &'a Bytes) -> Self {
        value.as_slice()
    }
}

impl<'a> UnsafeRefInto<BytesRef<'static>> for BytesRef<'a> {
    unsafe fn unsafe_into(self) -> BytesRef<'static> {
        std::mem::transmute(self)
    }
}

impl<'a> UnsafeRefInto<JsonRef<'static>> for JsonRef<'a> {
    unsafe fn unsafe_into(self) -> JsonRef<'static> {
        std::mem::transmute(self)
    }
}

impl<'a> EvaluableRef<'a> for JsonRef<'a> {
    const EVAL_TYPE: EvalType = EvalType::Json;
    type EvaluableType = Json;
    type ChunkedType = &'a ChunkedVecJson;

    #[inline]
    fn borrow_scalar_value(v: &'a ScalarValue) -> Option<Self> {
        match v {
            ScalarValue::Json(x) => x.as_ref().map(|x| x.as_ref()),
            _ => unimplemented!(),
        }
    }

    #[inline]
    fn borrow_scalar_value_ref(v: ScalarValueRef<'a>) -> Option<Self> {
        match v {
            ScalarValueRef::Json(x) => x,
            _ => unimplemented!(),
        }
    }

    #[inline]
    fn borrow_vector_value(v: &VectorValue) -> &ChunkedVecJson {
        match v {
            VectorValue::Json(x) => x,
            _ => unimplemented!(),
        }
    }

    #[inline]
    fn to_owned_value(self) -> Self::EvaluableType {
        self.to_owned()
    }

    #[inline]
    fn from_owned_value(value: &'a Json) -> Self {
        value.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64;

    #[test]
    fn test_bytes_as_bool() {
        let tests: Vec<(&'static [u8], Option<bool>)> = vec![
            (b"", Some(false)),
            (b" 23", Some(true)),
            (b"-1", Some(true)),
            (b"1.11", Some(true)),
            (b"1.11.00", None),
            (b"xx", None),
            (b"0x00", None),
            (b"11.xx", None),
            (b"xx.11", None),
            (
                b".0000000000000000000000000000000000000000000000000000001",
                Some(true),
            ),
        ];

        let mut ctx = EvalContext::default();
        for (i, (v, expect)) in tests.into_iter().enumerate() {
            let rb: Result<bool> = v.to_vec().as_mysql_bool(&mut ctx);
            match expect {
                Some(val) => {
                    assert_eq!(rb.unwrap(), val);
                }
                None => {
                    assert!(
                        rb.is_err(),
                        "index: {}, {:?} should not be converted, but got: {:?}",
                        i,
                        v,
                        rb
                    );
                }
            }
        }

        // test overflow
        let mut ctx = EvalContext::default();
        let val: Result<bool> = f64::INFINITY
            .to_string()
            .as_bytes()
            .to_vec()
            .as_mysql_bool(&mut ctx);
        assert!(val.is_err());

        let mut ctx = EvalContext::default();
        let val: Result<bool> = f64::NEG_INFINITY
            .to_string()
            .as_bytes()
            .to_vec()
            .as_mysql_bool(&mut ctx);
        assert!(val.is_err());
    }

    #[test]
    fn test_real_as_bool() {
        let tests: Vec<(f64, Option<bool>)> = vec![
            (0.0, Some(false)),
            (1.3, Some(true)),
            (-1.234, Some(true)),
            (0.000000000000000000000000000000001, Some(true)),
            (-0.00000000000000000000000000000001, Some(true)),
            (f64::MAX, Some(true)),
            (f64::MIN, Some(true)),
            (f64::MIN_POSITIVE, Some(true)),
            (f64::INFINITY, Some(true)),
            (f64::NEG_INFINITY, Some(true)),
            (f64::NAN, None),
        ];

        let mut ctx = EvalContext::default();
        for (f, expected) in tests {
            match Real::new(f) {
                Ok(b) => {
                    let r = b.as_mysql_bool(&mut ctx).unwrap();
                    assert_eq!(r, expected.unwrap());
                }
                Err(_) => assert!(expected.is_none(), "{} to bool should fail", f,),
            }
        }
    }
}
