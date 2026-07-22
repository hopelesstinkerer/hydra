//! Core architecture types: five-layer markers, phantom wrappers, and the
//! [`Layer`] trait.
//!
//! The five architectural layers (Orchestration, Retrieval, Memory, Context,
//! Execution) each have a phantom wrapper type and implement
//! [`TypeConstructor`] to link the marker to its wrapper. The [`Layer`] trait
//! unifies them under a common supertrait with an associated event type.

use std::fmt;
use std::marker::PhantomData;

use crate::algebra::TypeConstructor;
use crate::event::{Error, SystemEvent};
use crate::store::Repository;

// ---------------------------------------------------------------------------
// Wrapper macro
// ---------------------------------------------------------------------------

/// Define a phantom wrapper type with unconditional trait impls for `?Sized`
/// type parameters.
macro_rules! define_phantom_wrapper {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(PartialEq, Eq)]
        pub struct $name<T: ?Sized>(pub PhantomData<T>);

        impl<T: ?Sized> Copy for $name<T> {}

        impl<T: ?Sized> Clone for $name<T> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<T: ?Sized> Default for $name<T> {
            fn default() -> Self {
                $name(PhantomData)
            }
        }

        impl<T: ?Sized> fmt::Debug for $name<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.0).finish()
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Layer marker types
// ---------------------------------------------------------------------------

/// Marker type for the Orchestration layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Orchestration;

/// Marker type for the Retrieval layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Retrieval;

/// Marker type for the Memory layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Memory;

/// Marker type for the Context layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Context;

/// Marker type for the Execution layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Execution;

// ---------------------------------------------------------------------------
// Phantom wrapper types
// ---------------------------------------------------------------------------

define_phantom_wrapper!(Plan, "Phantom wrapper for the Orchestration layer.");
define_phantom_wrapper!(Index, "Phantom wrapper for the Retrieval layer.");
define_phantom_wrapper!(Record, "Phantom wrapper for the Memory layer.");
define_phantom_wrapper!(Window, "Phantom wrapper for the Context layer.");
define_phantom_wrapper!(Observation, "Phantom wrapper for the Execution layer.");

// ---------------------------------------------------------------------------
// TypeConstructor implementations (marker → wrapper)
// ---------------------------------------------------------------------------

impl TypeConstructor for Orchestration {
    type Of<T: ?Sized> = Plan<T>;
}

impl TypeConstructor for Retrieval {
    type Of<T: ?Sized> = Index<T>;
}

impl TypeConstructor for Memory {
    type Of<T: ?Sized> = Record<T>;
}

impl TypeConstructor for Context {
    type Of<T: ?Sized> = Window<T>;
}

impl TypeConstructor for Execution {
    type Of<T: ?Sized> = Observation<T>;
}

// ---------------------------------------------------------------------------
// is_composite_goal
// ---------------------------------------------------------------------------

/// Returns `true` if `goal` contains multiple instructions (v1 heuristic:
/// detects `;` or newline as separators).
///
/// Simple goals (single instruction) are executed directly. Composite
/// goals are decomposed into sub-goals during the Planning phase.
///
/// # Note
///
/// This heuristic is approximate — natural-language goals with
/// semicolons falsely split, and comma- or period-separated instructions
/// are missed.
#[must_use]
pub fn is_composite_goal(goal: &str) -> bool {
    goal.contains(';') || goal.contains('\n')
}

// ---------------------------------------------------------------------------
// EventPayload
// ---------------------------------------------------------------------------

/// The concrete payload type produced by a layer's [`process`](Layer::process)
/// method.
///
/// A type alias so the payload structure can evolve without changing
/// every signature. Currently [`String`].
pub type EventPayload = String;

// ---------------------------------------------------------------------------
// Layer trait
// ---------------------------------------------------------------------------

/// A trait for types that represent a system layer.
///
/// Types implementing `Layer` are type-level markers that identify which
/// architectural layer a value belongs to. Each layer has an associated
/// [`Event`](Self::Event) type and a [`process`](Self::process) method
/// that drives the layer's core logic.
pub trait Layer: TypeConstructor + Sized {
    /// The event type associated with this layer.
    type Event;

    /// Process an event for this layer.
    ///
    /// Takes a layer-specific event and a mutable reference to an event
    /// store, and returns a result containing:
    ///
    /// - An artifact (a wrapper around [`EventPayload`]) whose type is
    ///   determined by the layer's [`TypeConstructor`] implementation.
    /// - A vector of [`SystemEvent`]s emitted as side effects (may be
    ///   empty).
    fn process(
        &mut self,
        event: Self::Event,
        store: &mut dyn Repository<Event = SystemEvent>,
    ) -> Result<(Self::Of<EventPayload>, Vec<SystemEvent>), Error>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_impl_all;

    /// Verify that all five layer markers are constructible and `Sized`.
    #[test]
    fn markers_are_constructible_and_sized() {
        let _ = Orchestration;
        let _ = Retrieval;
        let _ = Memory;
        let _ = Context;
        let _ = Execution;

        fn _assert_sized<T: Sized>() {}
        _assert_sized::<Orchestration>();
        _assert_sized::<Retrieval>();
        _assert_sized::<Memory>();
        _assert_sized::<Context>();
        _assert_sized::<Execution>();
    }

    /// Verify that all five phantom wrappers are constructible via
    /// [`Default`].
    #[test]
    fn wrappers_constructible_via_default() {
        let _ = Plan::<()>::default();
        let _ = Index::<()>::default();
        let _ = Record::<()>::default();
        let _ = Window::<()>::default();
        let _ = Observation::<()>::default();
    }

    /// Compile-time assertions for wrapper Send/Sync and marker
    /// `TypeConstructor` bounds.
    #[test]
    fn compile_time_assertions() {
        // All wrappers are Send + Sync (with Sized type param)
        assert_impl_all!(Plan<()>: Send, Sync);
        assert_impl_all!(Index<()>: Send, Sync);
        assert_impl_all!(Record<()>: Send, Sync);
        assert_impl_all!(Window<()>: Send, Sync);
        assert_impl_all!(Observation<()>: Send, Sync);

        // All wrappers are Send + Sync with unsized type params
        assert_impl_all!(Plan<str>: Send, Sync);
        assert_impl_all!(Index<dyn std::fmt::Debug + Send + Sync>: Send, Sync);
        assert_impl_all!(Record<dyn std::fmt::Debug + Send + Sync>: Send, Sync);
        assert_impl_all!(Window<str>: Send, Sync);
        assert_impl_all!(Observation<dyn std::fmt::Debug + Send + Sync>: Send, Sync);

        // All markers implement TypeConstructor
        fn _tc<T: TypeConstructor>() {}
        _tc::<Orchestration>();
        _tc::<Retrieval>();
        _tc::<Memory>();
        _tc::<Context>();
        _tc::<Execution>();
    }

    /// Verify that wrappers with unsized type parameters are constructible.
    #[test]
    fn unsized_wrapper_construction() {
        let _p: Plan<str> = Plan(PhantomData);
        let _i: Index<dyn std::fmt::Debug + Send + Sync> = Index(PhantomData);
        let _r: Record<dyn std::fmt::Debug + Send + Sync> = Record(PhantomData);
        let _w: Window<str> = Window(PhantomData);
        let _o: Observation<dyn std::fmt::Debug + Send + Sync> = Observation(PhantomData);
    }

    // -----------------------------------------------------------------------
    // TestLayer — minimal layer implementation for testing process()
    // -----------------------------------------------------------------------

    /// A minimal layer implementation used to exercise [`Layer::process`]
    /// at runtime.
    ///
    /// `TestLayer` implements [`Layer<Event = ContextEvent>`](Layer) and
    /// always returns `Ok` with an empty side-effect list.
    struct TestLayer;

    impl TypeConstructor for TestLayer {
        type Of<T: ?Sized> = Window<T>;
    }

    impl Layer for TestLayer {
        type Event = crate::event::ContextEvent;

        fn process(
            &mut self,
            _event: crate::event::ContextEvent,
            _store: &mut dyn Repository<Event = SystemEvent>,
        ) -> Result<(Self::Of<EventPayload>, Vec<SystemEvent>), Error> {
            Ok((Window(PhantomData), vec![]))
        }
    }

    /// Verify that [`Layer::process`] returns `Ok` with the correct
    /// artifact type and an empty side-effect list.
    #[test]
    fn process_returns_ok() {
        use crate::event::ContextEvent;
        use crate::store::VecRepository;

        let mut layer = TestLayer;
        let mut repo: VecRepository<SystemEvent> = VecRepository::default();
        let event = ContextEvent;

        let (artifact, side_effects) = layer.process(event, &mut repo).unwrap();
        // Artifact should be Window<EventPayload> (i.e. Window<String>)
        let _: Window<EventPayload> = artifact;
        assert!(side_effects.is_empty());
    }
}
