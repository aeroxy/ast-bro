pub mod a {
    use crate::one as Api;

    pub struct AChild;

    impl Api::Base for AChild {}
}

pub mod b {
    // The same alias name, bound to a different module. Expanding the
    // qualifier with the sibling's alias would attach this to `one::Base`.
    use crate::two as Api;

    pub struct BChild;

    impl Api::Base for BChild {}
}
