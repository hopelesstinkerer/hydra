//! Algebraic foundation traits for type-level programming and data
//! composition.
//!
//! - [`TypeConstructor`] — higher-kinded type (HKT) encoding via generic
//!   associated types.
//! - [`Semigroup`] — an associative combine operation.
//! - [`Monoid`] — a semigroup with an identity element.
//! - Type-level wrapper types ([`Unit`], [`Sum`], [`Product`], [`Compose`])
//!   and marker types ([`Bottom`], [`Top`]) for compile-time composition.
//!
//! [`Semigroup`], [`Monoid`], [`Sum`], [`Product`], [`Compose`], [`Bottom`],
//! and [`Top`] provide type-level machinery for composing and combining
//! values. See GitHub issues #3 and #4 for the specification.

use std::marker::PhantomData;

// ---------------------------------------------------------------------------
// Algebraic traits
// ---------------------------------------------------------------------------

/// Higher-kinded type (HKT) encoding via generic associated types.
///
/// `TypeConstructor` abstracts over type constructors such as `Vec`, `Option`,
/// and `Box`. The associated type [`Of<T>`](Self::Of) applies the constructor
/// to `T`.
///
/// The `?Sized` bound on `T` enables application to trait objects, e.g.
/// [`Of<dyn Debug>`](Self::Of).
pub trait TypeConstructor {
    /// The type constructor applied to `T`.
    type Of<T: ?Sized>;
}

/// The associative combine operation of a semigroup.
///
/// # Laws
///
/// For all `a`, `b`, `c`:
/// - `a.combine(b).combine(c) == a.combine(b.combine(c))` (associativity)
pub trait Semigroup: Sized {
    /// Combine two values into one.
    #[must_use]
    fn combine(self, other: Self) -> Self;
}

/// A monoid is a [`Semigroup`] with an identity element.
///
/// # Laws
///
/// For all `a`:
/// - [`Self::identity()`].combine(a) == a (left identity)
/// - a.combine([`Self::identity()`]) == a (right identity)
pub trait Monoid: Semigroup {
    /// The identity element for this monoid.
    fn identity() -> Self;
}

// ---------------------------------------------------------------------------
// Type-level constructs (compile-time markers)
// ---------------------------------------------------------------------------

/// A unit type for type-level computations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit;

/// Type-level wrapper for additive interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sum<T>(pub T);

/// Type-level wrapper for multiplicative interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Product<T>(pub T);

/// Composition of two type constructors.
///
/// `Compose<F, G>` represents the type-level composition `F ∘ G`, i.e.
/// applying `F` after `G` to a type argument. The fields are
/// [`PhantomData`] because `Compose` is a compile-time marker — the
/// composition is resolved at the type level, never at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compose<F, G>(pub PhantomData<(F, G)>);

/// The uninhabited bottom type (no values).
///
/// An empty enum serves as the bottom type in stable Rust. No values of this
/// type can be constructed, which makes it analogous to the never type `!`.
pub enum Bottom {}

/// The top type (analogous to `()`).
pub type Top = ();

// ---------------------------------------------------------------------------
// TypeConstructor implementations
// ---------------------------------------------------------------------------

impl<T: ?Sized> TypeConstructor for PhantomData<T> {
    type Of<U: ?Sized> = PhantomData<U>;
}

impl<T: ?Sized> TypeConstructor for Box<T> {
    type Of<U: ?Sized> = Box<U>;
}

impl<F: TypeConstructor, G: TypeConstructor> TypeConstructor for Compose<F, G> {
    type Of<T: ?Sized> = F::Of<G::Of<T>>;
}

// ---------------------------------------------------------------------------
// Semigroup implementations
// ---------------------------------------------------------------------------

impl Semigroup for String {
    fn combine(mut self, other: Self) -> Self {
        self.push_str(&other);
        self
    }
}

impl<T> Semigroup for Vec<T> {
    fn combine(mut self, other: Self) -> Self {
        self.extend(other);
        self
    }
}

macro_rules! impl_semigroup_add {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl Semigroup for $ty {
                fn combine(self, other: Self) -> Self {
                    self + other
                }
            }
        )+
    };
}

impl_semigroup_add!(i32, i64, u32, u64);

impl<T: Semigroup> Semigroup for Option<T> {
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Some(a), Some(b)) => Some(a.combine(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Monoid implementations
// ---------------------------------------------------------------------------

impl Monoid for String {
    fn identity() -> Self {
        String::new()
    }
}

impl<T> Monoid for Vec<T> {
    fn identity() -> Self {
        Vec::new()
    }
}

macro_rules! impl_monoid_add {
    ($($ty:ty => $identity:expr),+ $(,)?) => {
        $(
            impl Monoid for $ty {
                fn identity() -> Self {
                    $identity
                }
            }
        )+
    };
}

impl_monoid_add!(
    i32 => 0i32,
    i64 => 0i64,
    u32 => 0u32,
    u64 => 0u64,
);

impl<T: Semigroup> Monoid for Option<T> {
    fn identity() -> Self {
        None
    }
}

// ---------------------------------------------------------------------------
// Law-checking helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod laws {
    //! Law-checking helpers for [`Semigroup`] and [`Monoid`] implementations.
    //!
    //! These helpers are `pub(crate)` so that other modules in the crate can
    //! verify algebraic law compliance in their own test suites.

    use super::*;
    use std::fmt::Debug;

    /// Assert that the semigroup associativity law holds for values `a`, `b`, `c`.
    ///
    /// Verifies: `(a <> b) <> c == a <> (b <> c)`
    pub fn check_semigroup_laws<T>(a: T, b: T, c: T)
    where
        T: Semigroup + Clone + PartialEq + Debug,
    {
        let left = a.clone().combine(b.clone()).combine(c.clone());
        let right = a.clone().combine(b.clone().combine(c.clone()));
        assert_eq!(
            left, right,
            "Semigroup associativity law violated for ({:?} <> {:?}) <> {:?} vs {:?} <> ({:?} <> {:?})",
            a, b, c, a, b, c,
        );
    }

    /// Assert that the monoid identity laws hold for value `a`.
    ///
    /// Verifies: `identity <> a == a` and `a <> identity == a`
    pub fn check_monoid_laws<T>(a: T)
    where
        T: Monoid + Clone + PartialEq + Debug,
    {
        let identity = T::identity();
        assert_eq!(
            identity.combine(a.clone()),
            a,
            "Monoid left identity law violated for identity <> {:?}",
            a,
        );
        assert_eq!(
            a.clone().combine(T::identity()),
            a,
            "Monoid right identity law violated for {:?} <> identity",
            a,
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::laws;

    #[test]
    fn string_semigroup_and_monoid() {
        laws::check_semigroup_laws("a".to_string(), "b".to_string(), "c".to_string());
        laws::check_monoid_laws("hello".to_string());
    }

    #[test]
    fn vec_i32_semigroup_and_monoid() {
        laws::check_semigroup_laws(vec![1, 2], vec![3, 4], vec![5]);
        laws::check_monoid_laws(vec![1, 2, 3]);
    }

    #[test]
    fn i32_semigroup_and_monoid() {
        laws::check_semigroup_laws(1i32, 2i32, 3i32);
        laws::check_monoid_laws(42i32);
    }

    #[test]
    fn i64_semigroup_and_monoid() {
        laws::check_semigroup_laws(10i64, 20i64, 30i64);
        laws::check_monoid_laws(100i64);
    }

    #[test]
    fn u32_semigroup_and_monoid() {
        laws::check_semigroup_laws(5u32, 10u32, 15u32);
        laws::check_monoid_laws(7u32);
    }

    #[test]
    fn u64_semigroup_and_monoid() {
        laws::check_semigroup_laws(100u64, 200u64, 300u64);
        laws::check_monoid_laws(42u64);
    }

    #[test]
    fn option_i32_semigroup_and_monoid() {
        laws::check_semigroup_laws(Some(1i32), Some(2i32), Some(3i32));
        laws::check_monoid_laws(Some(42i32));
        // Verify identity behavior with None
        laws::check_monoid_laws::<Option<i32>>(None);
    }

    #[test]
    fn option_semigroup_none_interaction() {
        // None as the first argument
        assert_eq!(None::<i32>.combine(Some(5i32)), Some(5i32));
        // None as the second argument
        assert_eq!(Some(5i32).combine(None::<i32>), Some(5i32));
        // Both None
        assert_eq!(None::<i32>.combine(None::<i32>), None::<i32>);
    }

    #[test]
    fn vec_concat_semantics() {
        let a = vec![1, 2, 3];
        let b = vec![4, 5];
        assert_eq!(a.combine(b), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn string_concat_semantics() {
        let a = "Hello, ".to_string();
        let b = "World!".to_string();
        assert_eq!(a.combine(b), "Hello, World!");
    }

    #[test]
    fn identity_does_not_panic() {
        // Calling identity() should never panic for any impl
        let _: String = Monoid::identity();
        let _: Vec<i32> = Monoid::identity();
        let _: i32 = Monoid::identity();
        let _: i64 = Monoid::identity();
        let _: u32 = Monoid::identity();
        let _: u64 = Monoid::identity();
        let _: Option<i32> = Monoid::identity();
    }

    #[test]
    fn type_constructor_box() {
        // Compile-time check that Box<dyn Debug>::Of<i32> is Box<i32>
        let _: <Box<dyn std::any::Any> as TypeConstructor>::Of<i32> = Box::new(42i32);
    }

    /// Verify that the TypeConstructor impl is object-safe for `?Sized` targets.
    #[test]
    fn type_constructor_unsized_target() {
        fn _assert<T: ?Sized>() {}
        // PhantomData<T>::Of<U: ?Sized> should accept unsized U
        _assert::<<PhantomData<i32> as TypeConstructor>::Of<dyn std::fmt::Debug>>();
    }

    /// Verify that `Compose<Box, Box>::Of<i32>` resolves to `Box<Box<i32>>`
    /// and that a `Compose` value is constructible.
    #[test]
    fn compose_type_constructor() {
        type ComposeBoxBox = Compose<Box<i32>, Box<i32>>;
        type Applied = <ComposeBoxBox as TypeConstructor>::Of<i32>;
        let _: Applied = Box::new(Box::new(42i32));
        // Also verify Compose value construction
        let _: Compose<Box<i32>, Box<i32>> = Compose(PhantomData::<(Box<i32>, Box<i32>)>);
    }
}
