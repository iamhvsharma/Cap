import { type NextRequest } from "next/server";
import { db } from "@cap/database";
import { videos } from "@cap/database/schema";
import { getCurrentUser } from "@cap/database/auth/session";
import { nanoId } from "@cap/database/helpers";
import { cookies } from "next/headers";

const allowedOrigins = [
  process.env.NEXT_PUBLIC_URL,
  "http://localhost:3001",
  "tauri://localhost",
  "http://tauri.localhost",
  "https://tauri.localhost",
];

export async function OPTIONS(req: NextRequest) {
  const params = req.nextUrl.searchParams;
  const origin = params.get("origin") || null;
  const originalOrigin = req.nextUrl.origin;

  return new Response(null, {
    status: 200,
    headers: {
      "Access-Control-Allow-Origin":
        origin && allowedOrigins.includes(origin)
          ? origin
          : allowedOrigins.includes(originalOrigin)
          ? originalOrigin
          : "null",
      "Access-Control-Allow-Credentials": "true",
      "Access-Control-Allow-Methods": "GET, OPTIONS",
      "Access-Control-Allow-Headers": "Authorization, sentry-trace, baggage",
    },
  });
}

export async function GET(req: NextRequest) {
  const token = req.headers.get("authorization")?.split(" ")[1];
  if (token) {
    cookies().set({
      name: "next-auth.session-token",
      value: token,
      path: "/",
      sameSite: "none",
      secure: true,
      httpOnly: true,
    });
  }

  const user = await getCurrentUser();
  const awsRegion = process.env.CAP_AWS_REGION;
  const awsBucket = process.env.CAP_AWS_BUCKET;
  const params = req.nextUrl.searchParams;
  const origin = params.get("origin") || null;
  const originalOrigin = req.nextUrl.origin;

  console.log("cookies:", cookies().getAll());

  if (!user) {
    return new Response(JSON.stringify({ error: true }), {
      status: 401,
      headers: {
        "Access-Control-Allow-Origin":
          origin && allowedOrigins.includes(origin)
            ? origin
            : allowedOrigins.includes(originalOrigin)
            ? originalOrigin
            : "null",
        "Access-Control-Allow-Credentials": "true",
        "Access-Control-Allow-Methods": "GET, OPTIONS",
        "Access-Control-Allow-Headers": "Authorization, sentry-trace, baggage",
      },
    });
  }

  const id = nanoId();
  const date = new Date();
  const formattedDate = `${date.getDate()} ${date.toLocaleString("default", {
    month: "long",
  })} ${date.getFullYear()}`;

  await db.insert(videos).values({
    id: id,
    name: `My Cap Recording - ${formattedDate}`,
    ownerId: user.userId,
    awsRegion: awsRegion,
    awsBucket: awsBucket,
  });

  if (
    process.env.NEXT_PUBLIC_IS_CAP &&
    process.env.NEXT_PUBLIC_ENVIRONMENT === "production"
  ) {
    const dubOptions = {
      method: "POST",
      headers: {
        Authorization: `Bearer ${process.env.DUB_API_KEY}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        url: process.env.NEXT_PUBLIC_URL + "/s/" + id,
        key: id,
      }),
    };

    await fetch("https://api.dub.co/links?projectSlug=cap", dubOptions)
      .then((response) => response.json())
      .then((response) => console.log(response))
      .catch((err) => console.error(err));
  }

  return new Response(
    JSON.stringify({
      id: id,
      user_id: user.userId,
      aws_region: awsRegion,
      aws_bucket: awsBucket,
    }),
    {
      status: 200,
      headers: {
        "Access-Control-Allow-Origin":
          origin && allowedOrigins.includes(origin)
            ? origin
            : allowedOrigins.includes(originalOrigin)
            ? originalOrigin
            : "null",
        "Access-Control-Allow-Credentials": "true",
        "Access-Control-Allow-Methods": "GET, OPTIONS",
        "Access-Control-Allow-Headers": "Authorization, sentry-trace, baggage",
      },
    }
  );
}
