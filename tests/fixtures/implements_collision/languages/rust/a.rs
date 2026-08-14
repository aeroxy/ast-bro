pub trait Root {}

pub trait Other {}

pub struct Leaf;

impl Root for Leaf {}

pub struct Hard<T>(pub T);

/// A generic impl, and a second trait on the same type.
impl<T> Root for Hard<T> {}

impl<T> Other for Hard<T> {}
