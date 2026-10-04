// The big-font modules are built ahead of their consumers (T-11 engine
// strip, T-12 renderer), so nothing in the binary calls them yet; the
// allow keeps the clippy gate green until then (see vault note N-3).
#[allow(dead_code)]
mod bigfont;
mod cli;
mod marquee;
mod renderer;
mod terminal;
mod unicode;

fn main() {
    let text = cli::input_text();
    let prepared = unicode::prepare(&text);
    let width = terminal::width();
    let engine = marquee::Engine::new(prepared, width);
    renderer::render_frame(&engine);
}
