# v0.9.1 public-surface readiness receipt

This branch finishes the v0.9.1 public-surface pass without changing the
released runtime or adding a website dependency.

## Repository state

- Base: `d9fdee8aec469915cfdc07ab40aba5c40e9e9de4` (`origin/main`, fetched 2026-07-21)
- Product truth: `508726960`
- Product-first homepage: `e37df06ca`
- Credential-free deploy preflight: `74862148a`
- No changes were pushed or deployed.

Open PRs were refreshed immediately before sign-off. PR #4673 changes the
shell command working-directory default and its tests; it does not overlap the
homepage, public-fact contract, screenshot assets, or empty-Work projection.
Draft PR #4508 remains the source record for the product-screenshot intent.

## Scope decisions

- Keep the existing Blue Stage visual direction instead of introducing a new
  design system.
- Describe a bounded path from task to verified change, not a perpetual loop.
- Use the existing whale component with a small CSS sun in the community
  section; no generated illustration is shipped.
- Use the real TUI capture from commit
  `5c3eb8245512cf790a933484453d3e300eb4c7af` as the homepage and README image.
  The two public copies share SHA-256
  `69c81df8a641cdad500d985973546db0a91c138e2c82e0de9586cdea7be85170`.

## Visual QA

The production build was inspected in English and Chinese at 1280x720 and
390x844. Both mobile pages reported a 390px layout viewport and 390px document
width, with no horizontal overflow. Keyboard traversal reached the wordmark,
locale switch, mobile menu, primary links, and copy control in document order.
A reduced-motion context reported no running animations. The Open Graph image
route returned HTTP 200 with `image/png`.

Artifacts:

- `docs/evidence/v091-home-desktop.png`
- `docs/evidence/v091-home-mobile.png`
- `docs/evidence/v091-home-zh-desktop.png`
- `docs/evidence/v091-home-zh-mobile.png`

## Verification

```text
npm run lint
npm test -- --run
  12 files passed; 83 tests passed
npm run check:facts
npm run check:docs
npm run check:deploy-env -- --preflight
npm run build
npx opennextjs-cloudflare build
bash scripts/release/check-versions.sh
git diff --check
```

The OpenNext Cloudflare worker bundle completed successfully. Production
deployment was intentionally not attempted: the local environment does not
contain the protected Cloudflare account ID or API token, and this task does
not authorize a push or deploy.
