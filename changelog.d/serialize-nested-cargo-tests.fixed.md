Fixed workspace test runs starving hostlib cases that intentionally launch
nested Cargo builds. Those real discovered-plan tests now share one bounded
nextest slot without weakening the suite-wide timeout.
