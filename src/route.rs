use crate::pages::homepage::HomePage;

use freya::prelude::*;
use freya_router::prelude::*;

#[derive(PartialEq)]
struct Layout {}
impl Component for Layout {
    fn render(&self) -> impl IntoElement {
        rect().center().expanded().child(Outlet::<Route>::new())
    }
}

#[derive(Routable, Clone, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    #[layout(Layout)]
    #[route("/")]
    HomePage,
}
