- **Release audits now reject missing lane prerequisites before expensive builds (#6365).** A missing
  `cargo-nextest`, Node.js, or ripgrep is reported immediately instead of after the warm build and shared CLI AOT
  preparation.
