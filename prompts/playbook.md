# Playbook

This is a handwritten document so please search and read through it as required. There won't be any specific order to things, we're just braindumping here.

Two repeating themes you'll see in this playbook are:

* Out of the box thinking
* Sparse use of screenshots or screen recordings.
* Reuse of everything

# Workflow

Our workflow is pretty straightforward:

* At the start of each day, the planner agent wakes up and builds a plan based on available metrics and data. This plan will be the source of truth for now. It will list all required assets, which all are missing, etc.
* The human user reviews the plan and steers it using the desk agent. After confirming edits, the human user requests the generation of any missing assets or manually uploads them.
* If an asset generation is requested, the chat agent will wake multiple asset generation subagents, each tasked with generating a single asset properly.
* Once all assets have been confirmed, the human user reviews again and tasks the chat agent to build all the videos. The chat agent spins up multiple video builder subagents to assemble each video.
* Once all videos are assembled, the human user reviews all videos and assigns the editor agent to make corrections, if any.
* Afterwards, the user manually schedules the posting of the videos by clicking the "Schedule" button.

The next day, this loop is repeated with new metrics and data.

# Strategy

Growth strategies largely depend on the current usage and scale of the app. RIght now, you'll mostly be dealing with small apps, ie, those with low user count. The strategy is optimized for that.

## Metrics

The app store won't report any relevant campaign data for small apps. So, we'll have to resort to guessing metrics. Here's what we'll do:

* On Instagram, we can't create a per post URL or identifier since IG doesn't support placing clickable links in the post description. Instead, users will have to click the link in bio to get to the app store listing. In this case, the most reliable metric for a post performing well is the **Profile visits** metric for the post. The more this increases, the more likely users are to reach the store listing.
* On Facebook, per post URL's are possible. So, we'll know which posts prompted the most number of CTA clicks and optimize from there.

But even these will have a significant delay to arrive. As such, metrics may not be available for all days. In case metrics are unavailable, resort to getting more views and profile visits for Instagram. For Facebook, keep measuring CTA clicks since they'll be available readily. When no data is available, always resort to optimizing for more views.

## Content strategy

For mobile apps, real content performs best. This applies even to later stage larger apps. Social media reacts well to human faces and real people as compared to cartoons, art, etc.

### Example 1

I'm not sure whether this will convey the message across to you. But here's a video that worked well for a screen time app:

* A girl is studying at her desk, or rather trying to study. She's visibly frustrated.
* She's distracted by her phone. A couple of screenshots showing her screen time at 3-5h per day are overlaid on the video with a low enough opacity to still see the girl.
* A caption appears then, *"I couldn't focus on studies, even before exam day..."*
* In the same scene, the girl's face is happy and she's studying.
* The phone is sitting face down on her desk. A couple of screenshots showing her screen time at 30-35 minutes per day are overlaid with the exact opacity.
* The caption now reads, *"But then I found \<app-name>"*.

Compared to a generic video introducing the product with the complete aesthetics of the product intact, this performed way way better. Note that in this video as well, the screenshots displayed were not of the product itself, but rather of the user's system generated screen time reports. Screenshots should be sparsely used but also relatable to viewers at a glance. People have used their system screen time reports so they're easily relatable. Whereas showing the screenshots or screen recordings of a product/app they've never seen is confusing.

### Example 2

Another video for an app that saves Instagram posts goes like this:

* In the first shot, a girl is sitting in a dark room and smiling. The camera is as though she's video calling the viewer, but without the video call controls.
* A caption reads "When did Instagram get this update!?" when showing her face.
* Camera cuts to showing her phone an the caption disappears.
* Instagram is open on her phone. She taps **Share** on an Instagram post and shares it to the app.
* The app saves it.

The actual app store link itself is in the bio. The fact that the link is in the bio is actually a pinned comment of the video. The comment reads, *"The app name is \<app-name> on the app store guys. Link is in bio."* . So once the user clicks share and doesn't find said option (since the app isn't installed), they'll naturally open the comment section to post a negative comment or check what others are saying. This is actually genius when compared to a generic video saying "Want to save your Instagram posts?" or "Are you searching for something you scrolled away?" because it prompts the user to actually interact with that same post by clicking **Share**. Instagram's algorithm values the clicks on the share button. If a certain post's share button is clicked a lot of times in the first few hours of posting it, then the algorithm boosts it significantly. Granted, this only works in specific contexts but is worth thinking about.

Also in general, these ads have to be optimized for views if no other metric is available. Make it such that it gets more views than the previous day. A content that's posted today is intended to reach a new audience, not yesterday's same ones. As such, no one cares even if you post the same content as yesterday with the almost same captions. Our only experiment is to improve views and CTA clicks. Often, subtle changes create the most effective results.

### Making videos

This section details our thoughts and observations on the actual videos themselves. Usually, natural and authentic looking UGC performs best as content on social media.

Here are some characteristics of natural & authentic videos:

* There will be enough space, like padding, around the main subject in the video to account for the platform's UI elements. More space at the top and bottom and a bit less but sufficient padding at the sides.
* To be precise, *padding* doesn't mean leaving blank space around the subject. It essentially means that the camera is a bit zoomed out such that there's enough space to account for said UI elements. Most AI generated and fake videos omit this and instead opt for close up shots. So opt for more zoomed out shots instead.
* Videos with voiceovers avoid long captions. At any given frame in the video, there shall be at most 4 words on screen. The message will be communicated via the voiceover, not writing. You can use text to render subtitles for the dialogue being spoken in the video. But even then, at most 3 words will appear as subtitles simultaneously.
* The above trait holds true for most non-voiceover content as well. A good video leaves much to guess and figure out for the viewer rather than explaining everything. For such text, human creators mostly use templates. You have access to templates yourself. Choose small phrases that invoke curiosity and use a template that best accomodates it.
* For smartphone app ads, screenshots of the app itself are never shown in the videos. Other screenshots in the video too are shown only if absolutely necessary, like in example 1 above. Otherwise, videos follow quirky formats, much similar to example 2.
* A very effective format is simply showing a relatable situation and solution (eg: I used to open instagram before bed and doomscroll till 2, then I found \<app name>). In this, there's no need to show any screenshot or screen recording of the app at all. Such videos get initial traction and are boosted by the algorithm, resulting in more views. For even more effectiveness, ditch voiceover & simply resort to showing two clips and two texts: (a) clip - a woman sitting in bed and scrolling on her phone with a clock beside her showing 2 AM as time, text - I used to doomscroll till 2, (b) clip - the same woman sleeping peacefully with the bedside lap switched off, same clock shows 11 PM as time, text - but then i found \<app>.
* Always keep the main video track in the editor populated with main video clips, not screenshots or other clips. If you want to overlay something or show something, use another video track. The main video should always be visible below so adjust the opacity of the element you're overlaying for effective see through.
* There shall be an actionable CTA written as text at the end of the video. Eg: **Link in bio**, **Link in description**, etc depending on the platform. Throughout the video, no frame should have more than one text element. Disclaimers and short notes are very problematic.

To produce content like those mentioned above, you can absolutely make reusable clips and reuse them across videos. This saves the user a lot of money as well.

# General tips

To perform best, you could follow some of these tips, as you see fit.

* Mostly single shot videos work the best. Example 1 & 2 only involve one or two camera angles at most.
* This is also the best to be generated with video generation models, as varying camera angles make them hallucinate. For instance, if a video has two shots, you could write checklist items for two separate videos. This way, if the user chooses to generate those videos instead of uploading, the video generation models won't hallucinate because each individual clip is straightforward and simple.
* Generating video assets costs the user significantly. So, it's always best to generate reusable video clips that can be reused across ideas. This doesn't mean you have to limit your creativity to comply strictly with this rule. Whenever possible, try to reuse existing video clips from assets and when generating, try to make it such that the clip can be reused in other ideas as well.
* You have a remotion editor at your disposal. With reusable assets, you can make variations of different videos across different days. 
* Social media platforms like Instagram don't care whether you've posted similar content previously. For instance, even if you cycle through 15 videos across 2-3 months, it's unlikely anyone would notice. Because our goal itself is to reach new users, not appeal to followers. Followers are mostly irrelevant in our case.
* Understand the platform and make sure the content conveys it. For instance, make sure that no creative in Facebook says *Link in bio* because the link is in the description. This only applies to Instagram, and also only when the creative is not a paid ad.
* Most people scroll their feed with the audio OFF. Assume that this is what they're seeing. If the audio is generally a voiceover of some version of the written text or caption, then replace that audio with some light music. If sound effects match something happening on the video, use that. You can have multiple audio clips in a single video.
* The app's full name is almost never required. The full name will contain suffixes added by the respective platforms. For eg: Haserupt: *Screen Time* is not the app's name, it's simply Haserupt. The suffix has to be omitted in videos. Show the concise memorable name of the app in the videos.

## Splitting work

Depending on what you are, you can be considerate about the other agents who'll take over your work. This way, all of you will be in harmony making the system work really well. For instance,

### Planning & desk agent

If you're the planner or the desk agent, keep in mind that:

* The video builder agent will need a full length video clip to go in the main track for each video.
* The video builder agent will need the timestamps at which text, if any, should be shown.
* The asset generation subagents should be able to understand the full context of the asset they're generating and how each asset fits into an individual video.
* A human chatting with you may say vague directions; so clear it up with them before editing the plan.
* Think about the future; whether an asset you decide to generate today will go obsolete.

Your job isn't merely to write a plan. It's to write a good enough plan that any human or agent reading it has clear and precise understanding of what you intended.

### Asset generator agent

If you're the asset generator, keep in mind that:

* Clips you generate shouldn't be one-time use.
* Any future agents can understand the concise description of every asset you save.
* You don't waste generation cycles on video generation models by accounting for their possiblity to hallucinate and tailoring the prompt to get the exact desired outcome.

Your clips should look good enough that the human shouldn't have to regenerate them.

### Video builder & editor agent

If you're the video builder or editor agent, keep in mind that:

* You should account for the limitations and capabilities of the video editor.
* The videos should look like authentic UGC content on social media
* You should learn and understand what content should look like from this playbook.
* You should build a video so well that it needn't be edited.

At the end of the day, your job is to make videos that are indistinguishable from normal content.
