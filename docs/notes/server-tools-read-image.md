# server/tools/read_image.rs

## read_image

`read_image{path}` is how a picture on this machine reaches the model: a
screenshot a `bash` call produced, a chart in the repository, a photo someone
dropped in the working directory.

The point is that the model **looks at** the picture. A tool that returned
base64 as text would be worse than useless: the model would receive a wall of
characters it cannot decode, and pay tokens for it. So the outcome carries an
`ImageRef` -- a path and a media type -- and the request builder turns that into
an `input_image` part. See `docs/notes/server-llm.md` for that half.

A relative `path` resolves against the session's working directory, because that
is what "the file I just made" means to a model that has been running `cd`.

## look

The whole tool except the plumbing, so the interesting paths are reachable
without a `ToolContext`.

The roster is asked first, before any file is read: a model that cannot see
pictures is refused with the roster entry named and the missing modality quoted.
That is the only refusal that tells the operator what to change; a provider's own
400 would not.

Then the file: unreadable is a message naming the path, and of unknown kind is a
message listing the extensions this tool recognises -- both are things the model
can act on, unlike "invalid request".

## media_type

The extension first, the magic number second: a file called `shot.PNG` is a PNG,
and a file with no extension is still a PNG if it starts with the PNG signature.

Certainty is the requirement. A wrong media type makes the provider reject the
request, and the model has no way to tell that from a broken tool.

## tests::a_model_with_no_image_modality_is_refused_by_name

The refusal names the call, the roster entry, and the modality list. That is the
whole content of the message: without those three, the operator has to guess.

## tests::a_picture_is_handed_over_by_path_with_its_media_type

The outcome points at the file; nothing is read twice and nothing is inlined at
this layer.

## tests::a_missing_file_says_which_path

A file that is not there is a tool result, not a panic and not a failed turn.

## tests::a_text_file_is_not_offered_as_a_picture

`notes.txt` is refused with the list of formats that would work, rather than
sent and rejected downstream.

## tests::the_extension_names_the_type_and_the_bytes_are_the_fallback

Case-insensitive extensions, and the signature when there is no usable one.
