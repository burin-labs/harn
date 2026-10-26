The bootstrap installer now uses the published XZ-capable runtime seed. Its
setup contract installs a published XZ release through the real public path,
so a gzip-only seed cannot silently qualify an XZ installation.
