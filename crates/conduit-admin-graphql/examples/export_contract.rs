//! Export the actual production root shape for committed contract fixtures.
fn main() {
    print!("{}", conduit_admin_graphql::build_admin_schema().sdl());
}
