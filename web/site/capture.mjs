import puppeteer from 'puppeteer';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';

const __dirname = dirname(fileURLToPath(import.meta.url));

async function capture(htmlFile, outFile, width = 360, height = 780) {
  const browser = await puppeteer.launch({ headless: true });
  const page = await browser.newPage();
  await page.setViewport({ width, height, deviceScaleFactor: 3 });
  await page.goto('file:///' + join(__dirname, htmlFile).replace(/\\/g, '/'), { waitUntil: 'networkidle0' });
  // Wait for animations to start
  await new Promise(r => setTimeout(r, 500));
  await page.screenshot({ path: join(__dirname, 'img', outFile), type: 'png' });
  await browser.close();
  console.log(`Captured ${outFile}`);
}

await capture('mockup-wallet.html', 'phone-wallet.png');
await capture('mockup-mining.html', 'phone-mining.png');
