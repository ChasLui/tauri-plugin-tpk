import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightClientMermaid from '@pasqal-io/starlight-client-mermaid';
import { ion } from 'starlight-ion-theme';

export default defineConfig({
  site: 'https://chaslui.github.io',
  base: '/tauri-plugin-tpk',
  integrations: [
    starlight({
      title: '\u{1F525}\u{1F504} tauri-plugin-tpk',
      description: 'Open-source OTA frontend updates for Tauri v2',
      plugins: [
        starlightClientMermaid(),
        ion({
          icons: { iconDir: './src/icons' },
          footer: {
            text: '\u{1F525}\u{1F504} ',
            links: [
              {
                text: 'GitHub',
                href: 'https://github.com/ChasLui/tauri-plugin-tpk',
              },
              {
                text: 'npm',
                href: 'https://www.npmjs.com/package/tauri-plugin-tpk-api',
              },
            ],
          },
        }),
      ],
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/ChasLui/tauri-plugin-tpk',
        },
      ],
      editLink: {
        baseUrl:
          'https://github.com/ChasLui/tauri-plugin-tpk/edit/main/',
      },
      customCss: ['./src/styles/custom.css'],
      sidebar: [
        { label: 'Introduction', slug: 'index' },
        { label: 'Readme', slug: 'readme' },
        {
          label: 'Start here',
          items: [
            { label: 'Philosophy', slug: 'philosophy' },
            { label: 'Configuration', slug: 'configuration' },
            { label: 'API Reference', slug: 'api-reference' },
            { label: 'Packaging', slug: 'packaging' },
          ],
        },
        {
          label: 'How it works',
          items: [
            { label: 'Architecture', slug: 'architecture' },
            { label: 'Overlay Resolution', slug: 'overlay' },
            { label: 'Disk Layout', slug: 'disk-layout' },
            { label: 'Server Contract', slug: 'server-contract' },
          ],
        },
        {
          label: 'Shipping it',
          items: [
            { label: 'Security', slug: 'security' },
            { label: 'App Review Checklist', slug: 'app-review-checklist' },
            { label: 'The Updater Boundary', slug: 'updater-boundary' },
            { label: 'Error Codes', slug: 'error-codes' },
            { label: 'Local Testing', slug: 'local-testing' },
          ],
        },
        {
          label: 'Migrating',
          items: [
            { label: 'From tauri-plugin-hotswap', slug: 'migrating-from-hotswap' },
          ],
        },
      ],
    }),
  ],
});
