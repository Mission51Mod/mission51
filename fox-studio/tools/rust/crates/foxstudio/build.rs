// Windows icon/version resources are compiled by the native SDK helper; Linux is a no-op.
#[path = "../../../release/embed-resource.rs"]
mod resource;

fn main() {
    resource::embed().expect("cannot embed Fox Studio Windows resources");
}
