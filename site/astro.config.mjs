import { defineConfig } from 'astro/config';

export default defineConfig({
  site: 'https://mattjmdesign.github.io',
  base: '/feeder',
  trailingSlash: 'always',
  build: {
    assets: '_astro',
  },
});
