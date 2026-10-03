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
