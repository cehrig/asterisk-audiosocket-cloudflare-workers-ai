export interface Env {
    AI: any;
}

export default {
    async fetch(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
        if (request.method === 'POST') {
            return await post(request, env);
        }

        return new Response('Hello World!');
    },
} satisfies ExportedHandler<Env>;

async function post(request: Request, env: Env) {
    // Receive WAVE
    const wave = new Uint8Array(await request.arrayBuffer());

    // Speech-to-Text
    const inputs = {
        audio: [...wave],
    };
    const input = await env.AI.run("@cf/openai/whisper-tiny-en", inputs);

    // Generate Response
    const messages = [
        {
            role: "system",
            content: "You are a telephone agent." +
                "Answer the caller short and sweet and don't use any code snippets in your answer. Make the answer 3 short sentences or less."
        },
        {
            role: "user",
            content: input.text,
        },
    ];
    const response = await env.AI.run("@cf/meta/llama-3.1-8b-instruct-fast", {messages});

    // Text-to-Speech
    const output = await env.AI.run("@cf/myshell-ai/melotts", {
        prompt: response.response,
        lang: "en"
    });

    return new Response(output.audio);
}