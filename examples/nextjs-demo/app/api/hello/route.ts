export async function GET() {
  return Response.json({ hello: "from next.js api routes on finite", at: new Date().toISOString() });
}
