import { describe, expect, it } from "vitest";

import { diskIoCharts } from "./diskio";

describe("diskIoCharts", () => {
  it("leaves out the chart the page already shows", () => {
    const charts = diskIoCharts("diskio", "monitorflow_diskio_busy_ratio");
    expect(charts.map((c) => c.title)).toEqual(["Read and written", "Reads and writes", "Average wait per I/O", "I/Os in flight"]);
    expect(charts[0]!.series).toEqual([
      { metric: "monitorflow_diskio_read_bytes_per_second", label: "Read" },
      { metric: "monitorflow_diskio_written_bytes_per_second", label: "Written" },
    ]);
  });

  it("keeps a pair when the page shows one of its metrics", () => {
    const charts = diskIoCharts("diskio", "monitorflow_diskio_reads_per_second");
    expect(charts).toHaveLength(5);
  });
});
