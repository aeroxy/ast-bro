use other::Widget as Root;

mod plain {
    // An ordinary import under an outer rename of the same name.
    use api::Root;

    pub struct Plain;

    impl Root for Plain {}
}
