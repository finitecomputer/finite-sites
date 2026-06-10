import { promises as fs } from "fs";

export const dynamic = "force-dynamic";

async function bumpCounter(): Promise<number> {
  const dataDir = process.env.DATA_DIR ?? ".";
  const path = `${dataDir}/visits.json`;
  let count = 0;
  try {
    count = JSON.parse(await fs.readFile(path, "utf8")).count;
  } catch {
    // first visit
  }
  count += 1;
  await fs.writeFile(path, JSON.stringify({ count }));
  return count;
}

export default async function Home() {
  const visits = await bumpCounter();
  return (
    <main style={{ maxWidth: "28rem", textAlign: "center", padding: "2rem" }}>
      <h1 style={{ fontSize: "1.4rem" }}>next.js on finite</h1>
      <p style={{ color: "#9a9aa8", lineHeight: 1.5 }}>
        Server-rendered by a Next.js standalone server in the platform
        sandbox. Rendered at {new Date().toISOString()} — visit #{visits},
        persisted in $DATA_DIR.
      </p>
      <p style={{ color: "#9a9aa8" }}>
        <a style={{ color: "#8b8bf0" }} href="/api/hello">/api/hello</a> is a live API route.
      </p>
    </main>
  );
}
