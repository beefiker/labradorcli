use super::*;

#[test]
fn test_repo_name() {
    assert_eq!(repo_name(Channel::Dev), "labrador-dev");
    assert_eq!(repo_name(Channel::Stable), "labrador");
}
