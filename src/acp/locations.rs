//! Visit only schema-defined locations. Source text, tool arguments, commands,
//! diagnostics, and extension metadata are opaque and must never be rewritten.
use super::{Command, Event, FileRequest, protocol as p};
use std::path::PathBuf;

pub enum Location<'a> { Path(&'a mut PathBuf), Uri(&'a mut String) }

fn content<E>(block: &mut p::ContentBlock, map: &mut impl FnMut(Location<'_>) -> Result<(), E>) -> Result<(), E> {
    match block {
        p::ContentBlock::ResourceLink(resource) => map(Location::Uri(&mut resource.uri))?,
        p::ContentBlock::Resource(resource) => match &mut resource.resource {
            p::EmbeddedResourceResource::TextResourceContents(resource) => map(Location::Uri(&mut resource.uri))?,
            p::EmbeddedResourceResource::BlobResourceContents(resource) => map(Location::Uri(&mut resource.uri))?,
            _ => {}
        },
        p::ContentBlock::Image(image) => { if let Some(uri) = &mut image.uri { map(Location::Uri(uri))?; } }
        _ => {}
    }
    Ok(())
}
fn tools<E>(items: &mut [p::ToolCallContent], map: &mut impl FnMut(Location<'_>) -> Result<(), E>) -> Result<(), E> {
    for item in items {
        match item {
            p::ToolCallContent::Diff(diff) => map(Location::Path(&mut diff.path))?,
            p::ToolCallContent::Content(item) => content(&mut item.content, map)?,
            _ => {}
        }
    }
    Ok(())
}
fn update<E>(update: &mut p::ToolCallUpdate, map: &mut impl FnMut(Location<'_>) -> Result<(), E>) -> Result<(), E> {
    if let Some(locations) = &mut update.fields.locations {
        for location in locations { map(Location::Path(&mut location.path))?; }
    }
    if let Some(items) = &mut update.fields.content { tools(items, map)?; }
    Ok(())
}
pub fn file_request<E>(request: &mut FileRequest, map: &mut impl FnMut(Location<'_>) -> Result<(), E>) -> Result<(), E> {
    map(Location::Path(match request { FileRequest::Read(request) => &mut request.path, FileRequest::Write(request) => &mut request.path }))
}
pub fn command<E>(command: &mut Command, map: &mut impl FnMut(Location<'_>) -> Result<(), E>) -> Result<(), E> {
    match command {
        Command::Prompt { content: blocks, .. } => { for block in blocks { content(block, map)?; } }
        Command::FileResponse { edit: Some(edit), .. } => map(Location::Path(&mut edit.path))?,
        _ => {}
    }
    Ok(())
}
pub fn event<E>(event: &mut Event, map: &mut impl FnMut(Location<'_>) -> Result<(), E>) -> Result<(), E> {
    match event {
        Event::FileRequest { request, .. } => file_request(request, map)?,
        Event::FileEdited(edit) => map(Location::Path(&mut edit.path))?,
        Event::Terminal(terminal) => map(Location::Path(&mut terminal.cwd))?,
        Event::SessionsListed(list) => {
            for session in &mut list.sessions {
                map(Location::Path(&mut session.cwd))?;
                for path in &mut session.additional_directories { map(Location::Path(path))?; }
            }
        }
        Event::Permission { request, .. } => update(&mut request.tool_call, map)?,
        Event::Update(notification) => match &mut notification.update {
            p::SessionUpdate::ToolCall(call) => {
                for location in &mut call.locations { map(Location::Path(&mut location.path))?; }
                tools(&mut call.content, map)?;
            }
            p::SessionUpdate::ToolCallUpdate(call) => update(call, map)?,
            p::SessionUpdate::UserMessageChunk(chunk) | p::SessionUpdate::AgentMessageChunk(chunk) | p::SessionUpdate::AgentThoughtChunk(chunk) => content(&mut chunk.content, map)?,
            _ => {}
        },
        _ => {}
    }
    Ok(())
}
