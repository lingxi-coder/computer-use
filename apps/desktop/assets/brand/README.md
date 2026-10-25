# LingXi Control / 灵犀操控

Brand artwork is copied from `lingxi-app/assets/brand/identity-v1/products/computer-use`.
`icon.svg` is the full application mark; `small.svg` omits the small pointer for
compact UI. `icon.png` is a 1024 px export of `icon.svg`; `icon.icns` is its macOS
icon container. The app, renderer and local macOS packager use these files.

The display name is LingXi Control. Bundle ID `com.lingxi.computer-use`, npm
identifiers, Keychain service identifiers and the established `Computer Use`
user-data directory stay unchanged. The red tray Stop control retains its
existing safety affordance and uses the new product name in its tooltip.

`scripts/sign-macos-local-app.mjs` signs a hash-pinned historical Computer Use
artifact, so its identity must remain unchanged. It does not sign newly built
LingXi Control bundles; fresh signing/release acceptance is a separate task.
