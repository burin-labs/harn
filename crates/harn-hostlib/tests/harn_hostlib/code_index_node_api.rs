use harn_hostlib::code_index::{Node, NodeKind};

#[test]
fn public_constructor_supports_optional_source_metadata() {
    let mut node = Node::new(NodeKind::Function, "load", 7, "Store.java", 3, "java");
    assert_eq!(node.id, 0);
    assert_eq!(node.kind, NodeKind::Function);
    assert_eq!(node.name, "load");
    assert_eq!(node.file_id, 7);
    assert_eq!(node.path.as_ref(), "Store.java");
    assert_eq!(node.line, 3);
    assert_eq!(node.language, "java");
    assert_eq!(node.col, None);
    assert_eq!(node.owner_type, None);
    assert!(node.signature.is_empty());
    assert_eq!(node.container, None);
    assert_eq!(node.access_level, None);
    node.col = Some(12);
    node.owner_type = Some("lab.Store".into());
    node.signature = "int load(int id)".into();
    assert_eq!(node.col, Some(12));
    assert_eq!(node.qualified_name(), "lab.Store.load");
}
