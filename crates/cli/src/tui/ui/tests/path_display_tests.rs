
use super::super::shorten_path_with_home;
use std::path::Path;

#[test]
fn shortens_home_and_children_only_on_path_boundaries() {
    let home = Path::new("/home/ryan");

    assert_eq!(shorten_path_with_home(Path::new("/home/ryan"), home), "~");
    assert_eq!(
        shorten_path_with_home(Path::new("/home/ryan/project"), home),
        "~/project"
    );
    assert_eq!(
        shorten_path_with_home(Path::new("/home/ryan2/project"), home),
        "/home/ryan2/project"
    );
}
