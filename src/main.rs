fn main() -> anyhow::Result<()> {
    jjfx::run(std::env::args().skip(1).collect())
}
