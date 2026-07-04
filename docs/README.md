# smolbren docs

Mintlify docs site for [smolbren](https://github.com/junaidrahim/smolbren).
Navigation and site config live in [`docs.json`](docs.json); pages are MDX.

## Local development

```sh
npm i -g mint
mint dev        # run from this directory (where docs.json lives)
```

Preview at http://localhost:3000. Changes deploy automatically when pushed to
`main` via the Mintlify GitHub app.
