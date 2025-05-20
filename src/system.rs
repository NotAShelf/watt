pub struct System {
    pub is_desktop: bool,
}

impl System {
    pub fn new() -> anyhow::Result<Self> {
        let mut system = Self { is_desktop: false };
        system.rescan()?;

        Ok(system)
    }

    pub fn rescan(&mut self) -> anyhow::Result<()> {}
}
