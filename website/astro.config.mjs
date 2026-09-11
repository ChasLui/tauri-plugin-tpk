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
          label: 'Guides',
          items: [
            { label: 'Configuration', slug: 'configuration' },
            { label: 'Creating Bundles', slug: 'creating-bundles' },
            { label: 'Server Contract', slug: 'server-contract' },
            { label: 'Local Testing', slug: 'local-testing' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'API Reference', slug: 'api-reference' },
            { label: 'Architecture', slug: 'architecture' },
            { label: 'Advanced Policies', slug: 'advanced-policies' },
            { label: 'Security', slug: 'security' },
            { label: 'Design Philosophy', slug: 'philosophy' },
          ],
        },
      ],
    }),
  ],
});
