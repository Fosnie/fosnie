# ECharts option snippets

Ready `setOption({…})` blocks. Write the **data + options**; the library is inlined
at `<!-- pai:echarts -->`. Always set `color: palette` (read from the theme variables
`--pai-c1…--pai-c6`) so charts match the house palette. Each chart needs a sized
container (`<div class="pai-chart" id="…"></div>`) and one `echarts.init(el)`.

```js
const css = getComputedStyle(document.documentElement);
const palette = ['--pai-c1','--pai-c2','--pai-c3','--pai-c4','--pai-c5','--pai-c6']
  .map(v => css.getPropertyValue(v).trim() || '#2f6db5');
```

## Bar (categorical)

```js
echarts.init(el).setOption({
  color: palette,
  tooltip: { trigger: 'axis' },
  grid: { left: 48, right: 16, top: 24, bottom: 32 },
  xAxis: { type: 'category', data: ['Litigation','Regulatory','Contract'] },
  yAxis: { type: 'value' },
  series: [{ type: 'bar', data: [21, 13, 8], barWidth: '52%',
             itemStyle: { borderRadius: [4,4,0,0] } }]
});
```

## Line / area (trend)

```js
echarts.init(el).setOption({
  color: palette,
  tooltip: { trigger: 'axis' },
  xAxis: { type: 'category', boundaryGap: false, data: ['Q3','Q4','Q1','Q2'] },
  yAxis: { type: 'value' },
  series: [{ type: 'line', smooth: true, areaStyle: { opacity: 0.15 }, data: [3.1,3.6,3.9,4.2] }]
});
```

## Pie / donut (composition)

```js
echarts.init(el).setOption({
  color: palette,
  tooltip: { trigger: 'item', formatter: '{b}: {c} ({d}%)' },
  legend: { bottom: 0 },
  series: [{ type: 'pie', radius: ['45%','70%'], avoidLabelOverlap: true,
             label: { show: false }, data: [
    { name: 'Litigation', value: 21 }, { name: 'Regulatory', value: 13 }, { name: 'Contract', value: 8 }
  ]}]
});
```

## Stacked bar (parts over categories)

```js
echarts.init(el).setOption({
  color: palette,
  tooltip: { trigger: 'axis' },
  legend: { top: 0 },
  xAxis: { type: 'category', data: ['Q1','Q2','Q3','Q4'] },
  yAxis: { type: 'value' },
  series: [
    { name: 'Open',   type: 'bar', stack: 't', data: [5,7,6,8] },
    { name: 'Closed', type: 'bar', stack: 't', data: [3,4,5,4] }
  ]
});
```

## Gauge (single KPI, 0–100)

```js
echarts.init(el).setOption({
  series: [{ type: 'gauge', progress: { show: true }, max: 100,
             axisLine: { lineStyle: { width: 12 } },
             detail: { valueAnimation: true, formatter: '{value}%' },
             data: [{ value: 72 }] }]
});
```

## Horizontal bar (ranking — long labels)

```js
echarts.init(el).setOption({
  color: palette,
  grid: { left: 120, right: 16, top: 8, bottom: 8 },
  xAxis: { type: 'value' },
  yAxis: { type: 'category', data: ['Contract','Regulatory','Litigation'] },
  series: [{ type: 'bar', data: [8,13,21] }]
});
```

## Responsiveness

Resize charts with the viewport (optional):

```js
const charts = [];                 // push each echarts.init(...) instance
addEventListener('resize', () => charts.forEach(c => c.resize()));
```
