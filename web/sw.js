// Minimal service worker — required for PWA installability.
// No offline caching; all requests are passed through to the network.

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));
