use super::*;
use std::io::Write;

#[test]
fn read_file_offset_and_limit() {
    let dir = std::env::temp_dir().join("harn_test_read_file_offset");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_offset.txt");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=20 {
            writeln!(f, "line {i}").unwrap();
        }
    }
    let path_str = path.to_str().unwrap();

    let result = handle_tool_locally("read_file", &json!({"path": path_str})).unwrap();
    assert!(result.contains("1\tline 1"), "first line numbered");
    assert!(result.contains("20\tline 20"), "last line numbered");
    assert!(!result.contains("more lines not shown"), "no truncation");

    let result = handle_tool_locally(
        "read_file",
        &json!({"path": path_str, "offset": 5, "limit": 3}),
    )
    .unwrap();
    assert!(result.contains("5\tline 5"), "starts at line 5");
    assert!(result.contains("7\tline 7"), "ends at line 7");
    assert!(!result.contains("4\tline 4"), "no line 4");
    assert!(!result.contains("8\tline 8"), "no line 8");
    assert!(result.contains("more lines not shown"), "truncation hint");
    assert!(result.contains("offset=8"), "hint says offset=8");

    let result =
        handle_tool_locally("read_file", &json!({"path": path_str, "offset": 100})).unwrap();
    assert!(!result.contains("line"), "no content past end");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}
