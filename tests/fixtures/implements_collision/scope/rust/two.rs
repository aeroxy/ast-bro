pub mod a {
    use crate::api::Target as Alias;
    use external::Target as Outside;

    pub struct AChild;

    impl Alias for AChild {}

    pub struct AOutside;

    impl Outside for AOutside {}
}

pub mod b {
    // A rename inside a sibling module is not in scope here.
    pub trait Alias {}

    pub struct BChild;

    impl Alias for BChild {}
}
