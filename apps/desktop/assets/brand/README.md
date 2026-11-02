# LingXi Control / 灵犀操控

Brand artwork is copied from `lingxi-app/assets/brand/identity-v1/products/computer-use`.
`icon.svg` and `small.svg` use the same existing sparkle mark (✨), with no
product glyphs. Product variants share this shape and differ only in color;
Control retains purple `#6653B9`. `icon.png` is a 1024 px export of `icon.svg`
with the existing full-canvas rounded background; `icon.icns` is its macOS
icon container. The app, renderer and local macOS packager use these files.

The display name is LingXi Control. Bundle ID `com.lingxi.computer-use`, npm
identifiers, Keychain service identifiers and the established `Computer Use`
user-data directory stay unchanged. The red tray Stop control retains its
existing safety affordance and uses the new product name in its tooltip.

`scripts/sign-macos-local-app.mjs` signs a hash-pinned historical Computer Use
artifact, so its identity must remain unchanged. It does not sign newly built
LingXi Control bundles; fresh signing/release acceptance is a separate task.

## Asset SHA-256

Generated from the checked-in sparkle sources; verify these assets before packaging.

| File | SHA-256 |
| --- | --- |
| `icon.svg` | `857a374719ec48818d3867dc678e38a0e638a2c5a900fa5a858d8d5815a115fb` |
| `small.svg` | `857a374719ec48818d3867dc678e38a0e638a2c5a900fa5a858d8d5815a115fb` |
| `icon.png` | `d424281604aa4c6165de412d6538c930d30d02055b878dbe6d6e3063ada359e6` |
| `icon.icns` | `6bd387db7b000349a9f33fc8b739864b6ebfd81b798e63b6d37fd7eb096e49a7` |
