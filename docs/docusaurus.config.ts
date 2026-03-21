import type {Config} from '@docusaurus/types';
import {themes as prismThemes} from 'prism-react-renderer';

const baseUrl = process.env.DOCUSAURUS_BASE_URL ?? '/';
const config: Config = {
  title: 'Odyssey',
  tagline: 'Portable agent runtime docs for operators and Rust integrators.',
  favicon: 'img/logo.png',
  url: 'https://liquidos-ai.github.io',
  baseUrl,
  organizationName: 'liquidos-ai',
  projectName: 'Odyssey',
  onBrokenLinks: 'throw',
  trailingSlash: true,
  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'throw',
    },
  },
  themes: [],
  presets: [
    [
      'classic',
      {
        docs: {
          path: 'content',
          routeBasePath: '/',
          sidebarPath: './sidebars.ts',
          editUrl: 'https://github.com/liquidos-ai/Odyssey/tree/main/docs/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      },
    ],
  ],
  themeConfig: {
    image: 'img/logo.png',
    colorMode: {
      defaultMode: 'light',
      disableSwitch: false,
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'LiquidOS',
      hideOnScroll: true,
      logo: {
        alt: 'LiquidOS Odyssey Platform',
        src: 'img/logo.svg',
        width: 24,
        height: 24,
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docsSidebar',
          position: 'left',
          label: 'Docs',
        },
        {
          to: '/getting-started/',
          label: 'Getting Started',
          position: 'left',
        },
        {
          href: 'https://docs.rs/releases/search?query=odyssey-rs',
          label: 'docs.rs',
          position: 'left',
        },
        {
          href: 'https://github.com/liquidos-ai/Odyssey',
          position: 'right',
          className: 'navbar-github-link',
          'aria-label': 'GitHub Repository',
        },
      ],
    },
    footer: {
      style: 'light',
      links: [
        {
          title: 'Docs',
          items: [
            {
              label: 'Overview',
              to: '/',
            },
            {
              label: 'Getting Started',
              to: '/getting-started/',
            },
            {
              label: 'Runtime Model',
              to: '/runtime-model/',
            },
          ],
        },
        {
          title: 'Reference',
          items: [
            {
              label: 'docs.rs',
              href: 'https://docs.rs/releases/search?query=odyssey-rs',
            },
            {
              label: 'Bundle Format',
              to: '/bundle-format/',
            },
            {
              label: 'Sandbox And Tools',
              to: '/sandbox-and-tools/',
            },
          ],
        },
        {
          title: 'Project',
          items: [
            {
              label: 'Repository',
              href: 'https://github.com/liquidos-ai/Odyssey',
            },
            {
              label: 'Contributing',
              href: 'https://github.com/liquidos-ai/Odyssey/blob/main/CONTRIBUTING.md',
            },
          ],
        },
      ],
      copyright: `Copyright ${new Date().getFullYear()} <a href="https://liquidos.ai" target="_blank" rel="noopener noreferrer">LiquidOS AI</a>`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['bash', 'json', 'yaml', 'toml', 'rust'],
    },
  },
};

export default config;
