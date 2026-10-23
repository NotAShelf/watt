# Configuring Watt

Configure Watt with TOML rules. Each `[[rule]]` requires a unique `name` and
`priority`. A rule can have an `if` condition and one or more action sections.
Watt evaluates rules from highest to lowest priority. When several matching
rules set the same option, the highest-priority value wins. Lower-priority rules
fill settings that higher-priority rules leave unset.

## Configuration reference

- [Loading, verification and daemon administration](configuration/administration.md)
- [Rules and expressions](configuration/rules.md)
- [Actions and targets](configuration/actions.md)
- [Hardware support and changes](configuration/hardware.md)
