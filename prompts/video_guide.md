# A Practical Guide to Prompting Video Models

*Checked October 18, 2026. Model versions and features can differ by app or provider. This guide focuses on Seedance 2.5, Kling 4.0, Veo 3.1, LTX 2.5, and the open-source Wan 2.2 family.*

## The short version

A video model doesn’t know what you meant but forgot to say. When a prompt leaves gaps, it fills them with something that seems likely: an extra camera move, a changed face, a new object, or dialogue you never asked for. That’s a practical explanation of these errors, not a diagnosis vendors give for each mistake. Conflicting references, long action sequences, and complicated physical interactions give the model more chances to guess.

For fewer surprises, specify:

1. **What stays the same:** identity, clothes, location, framing.
2. **What changes:** one main action, in order.
3. **What the camera does:** follows the subject, stays locked, or moves in a named way.
4. **What the sound is:** give exact dialogue in quotes, or say what ambience or music you want.
5. **What references are for:** identity, movement, camera, setting, or style.

Use controls and references when the app offers them. Don’t ask prose alone to do a job a keyframe, reference image, or motion track can do better.

In our earlier prompts, the Laya selfie shot got clearer once we spelled out the camera’s point of view, kept the phone out of frame, made its screen the only light, and described her expression changing gradually. In the LTX car example, the key insight was that start and end images show two visual states; they don’t specify the car’s path and the camera’s path separately.

## Seedance — give each reference a job

Seedance 2.5 is built for longer stories, audio-video generation, editing, and combinations of image, video, and audio references. Its official guide describes generation up to 30 seconds and support for multiple reference assets. The team also says complex physics and interactions still have room to improve. [Seedance 2.5 overview](https://seed.bytedance.com/en/blog/one-take-creation-flexible-referencing-introducing-seedance-2-5)

**Common surprise:** If two references disagree, the model may blend them in an unwanted way. Say what to borrow from each—and what to leave out.

**Bad:** “Use these references to make Laya scrolling on her phone. Make it cinematic.”

**Better:** “Use `@Image1` only for Laya’s face, hair, and clothes. One continuous close-up from the perspective of her phone’s front camera; keep the phone and her hands outside the frame. The screen is the only light. She scrolls while looking tired, then stops and slowly looks up. Preserve her identity and the dark room. No cuts or extra light sources.”

Use the platform’s actual reference labels. For image-to-video, describe the movement you want; the image already supplies much of the appearance.

## Kling — take advantage of references, but control the director

Kling’s current guide separates **Omni Reference**, for combining assets and ideas, from **Keyframes**, for fixing important visual moments. Kling 4.0 supports multi-shot storytelling, and its guide says multi-shot is on by default. A simple or abstract prompt may be expanded with extra story beats or camera choices. [Kling 4.0 guide](https://kling.ai/quickstart/kling-4-model-user-guide)

**Common surprise:** The model may decide how to stage a vague idea. “Cinematic” or “premium” doesn’t tell it what should actually be in frame.

**Bad:** “A woman films a selfie, looks up, smiles. Cinematic and emotional.”

**Better:** “One continuous front-camera close-up, no cuts. The woman looks down at her phone, then stops scrolling and looks into the lens. Her tired expression becomes a small, warm smile. Keep her face and clothes unchanged; keep the phone outside the frame. The camera has slight natural hand movement.”

For a precise sequence, use keyframes if available. For multiple assets, spell out each asset’s role. Kling’s guide explicitly recommends concrete visual details over abstract terms and says longer prompts aren’t automatically better. [Kling 4.0 guide](https://kling.ai/quickstart/kling-4-model-user-guide)

## Veo — direct the picture and the sound

Veo 3.1 supports audiovisual generation. Google’s prompt guidance recommends describing framing, camera motion, style, lighting, character, location, action, and dialogue. Depending on the interface, you may also be able to use reference images or first-and-last frames. [Veo prompt guide](https://deepmind.google/models/veo/prompt-guide/), [Google Cloud’s Veo 3.1 guide](https://cloud.google.com/blog/products/ai-machine-learning/ultimate-prompting-guide-for-veo-3-1/)

**Common surprise:** If you leave sound vague, you may get plausible but unwanted speech, music, or effects. If the words matter, write them exactly.

**Bad:** “A tired woman looks at the camera and says something reassuring. Soft music.”

**Better:** “Close-up, fixed front-facing phone-camera view. A tired woman looks down, stops scrolling, then looks into the lens and says, ‘I’m ready to put my phone down.’ The phone screen softly lights her face. Audio: quiet room tone, no music, no other voices.”

If an exact transition matters, use first-and-last-frame controls when your Veo interface supports them. Don’t assume every feature is available in every product or API.

## LTX — write the action as a clear sequence

LTX’s open-source 2.5 guide favors a focused scene with clear action and camera language. For image-to-video, the image supplies much of the visual setup, so focus the text on what changes. LTX can generate synchronized sound; its optional prompt enhancer may rewrite a prompt in some local workflows. Its docs caution that exact on-screen text and chaotic physics remain uneven. [LTX prompting guide](https://docs.ltx.io/open-source-model/usage-guides/prompting-guide/), [LTX image-to-video guide](https://docs.ltx.io/open-source-model/usage-guides/image-to-video/)

**Common surprise:** A prompt with lots of visual adjectives but few verbs gives the model little to animate. Two keyframes don’t tell it exactly how the subject or camera travels between them.

**Bad:** “A cinematic, realistic, beautiful car scene with dramatic lighting.”

**Better:** “A red car drives steadily around a racetrack. The camera tracks beside it, keeping the car in frame as the background moves past. The car stays the same shape and color. Engine and tire sounds continue; no music.”

For a car moving through several keyframes, describe the car’s motion separately from the camera’s motion. If you need tight structural control, use LTX’s motion, depth, pose, or other compatible conditioning workflow instead of asking text to infer all of it. In local workflows, check whether prompt enhancement is on if you need to preserve your wording exactly.

## Wan — match the prompt to the specific task

“Wan” covers several models and workflows. The Wan 2.2 repository lists separate text-to-video, image-to-video, speech-to-video, and character-animation models. It also offers optional prompt extension, which can add detail before generation. [Official Wan 2.2 repository](https://github.com/Wan-Video/Wan2.2)

**Common surprise:** Advice for one checkpoint may not fit another. And if prompt extension is enabled, the final prompt may contain details you didn’t write.

**Bad:** “Make this photo into a cool video. Add movement and atmosphere.”

**Better:** “Keep the person and room as shown in the input image. She slowly turns her head toward the window. A light breeze moves a few strands of hair. The camera stays still; the lighting and clothing do not change.”

Start with a plain prompt and compare it with prompt extension enabled. In the open-source workflow, negative-prompt and sampling controls are separate from the text prompt; change one setting at a time so you can tell what helped. [Wan 2.2 generation code and prompt-extension instructions](https://github.com/Wan-Video/Wan2.2)

## A quick check before you generate

Ask yourself:

- Did I give the scene **one main action**, in the order it happens?
- Did I distinguish **camera movement** from **subject movement**?
- Did I say what a reference should contribute?
- Did I make clear what must stay unchanged?
- Did I write the exact dialogue or sound I need?
- Am I asking for something the model can’t reliably show, like perfect tiny text or a complicated physical sequence?

If the last answer is yes, simplify the shot or use a more precise control. Generate a short version first, then add detail only when it fixes a specific problem.