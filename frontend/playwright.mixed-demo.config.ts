import {defineConfig} from '@playwright/test';
export default defineConfig({testDir:'./tests/local-routing-demo',testMatch:'mixed.spec.ts',workers:1,retries:0,timeout:180000,
outputDir:process.env.FOSNIE_MIXED_OUTPUT+'/browser-results',use:{baseURL:'http://127.0.0.1:5190',headless:true,channel:'msedge'}});
