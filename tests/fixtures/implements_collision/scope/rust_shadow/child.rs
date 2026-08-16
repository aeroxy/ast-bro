use api::Root as Base;

mod inner {
    use other::Root as Base;

    pub struct Child;

    impl Base for Child {}
}
