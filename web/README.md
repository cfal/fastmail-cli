# Embedded GraphiQL

The server embeds the compressed assets in `dist/`; browsers load no CDN code.
Queries, results and headers are not persisted to browser storage. Dependency
versions and package integrity hashes are recorded in `package-lock.json`.

To update assets using Node 24 and npm 11:

```sh
cd web
npm ci --ignore-scripts
npm audit
npm run build
```

Commit the source, lockfile and generated `dist/` together. Cargo builds need
neither Node nor network access to build these assets. `dist/SHA256SUMS` records
the compressed files; `dist/THIRD_PARTY_NOTICES.txt` contains bundled licenses.

The explorer's upstream package still declares React 15/16 peer dependencies;
its maintained GraphiQL adapter supports React 19. npm reports that stale peer
range during installation. Browser smoke tests must cover the explorer when
updating these packages.

The scroll-bar package omits its license from the npm tarball. Its license in
`licenses/` is upstream Git blob `7c08c3990396ecefd90f99ff5d9a34f26f5b5616` from
`theKashey/react-remove-scroll-bar`. Other monorepo packages that omit a license
use their repository's license shipped in the corresponding root package.
