fn main() -> anyhow::Result<()> {
    leio_code::doctors::native::serve(leio_self_doctors::doctors::registry())
}
