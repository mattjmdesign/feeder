import react from '@astrojs/react';
import { defineConfig } from 'astro/config';

export default defineConfig({
  site: 'https://mattjmdesign.github.io',
  base: '/feeder',
  trailingSlash: 'always',
  integrations: [react()],
  build: {
    assets: '_astro',
  },
});
