import type { CSSProperties, ReactNode } from 'react';
import {
  PIXEL_AVATARS,
  type PixelAccessory,
  type PixelHairStyle,
} from './pixel-avatars';

export type PixelAvatarPose = 'bust' | 'seat' | 'walk';
export type PixelAvatarFacing = 'north' | 'south' | 'east' | 'west';

function Px({
  className,
  x,
  y,
  w = 1,
  h = 1,
}: {
  className: string;
  x: number;
  y: number;
  w?: number;
  h?: number;
}) {
  return <rect className={className} x={x} y={y} width={w} height={h} />;
}

function BustHair({ style }: { style: PixelHairStyle }): ReactNode {
  switch (style) {
    case 'crop':
      return (
        <>
          <Px className="pixel-avatar-ink" x={5} y={3} w={6} h={1} />
          <Px className="pixel-avatar-hair" x={5} y={3} w={6} h={3} />
          <Px className="pixel-avatar-hair" x={4} y={5} w={2} h={3} />
          <Px className="pixel-avatar-hair" x={10} y={5} w={2} h={2} />
        </>
      );
    case 'curls':
      return (
        <>
          <Px className="pixel-avatar-hair" x={4} y={3} w={2} h={2} />
          <Px className="pixel-avatar-hair" x={6} y={2} w={2} h={2} />
          <Px className="pixel-avatar-hair" x={8} y={3} w={2} h={2} />
          <Px className="pixel-avatar-hair" x={10} y={2} w={2} h={3} />
          <Px className="pixel-avatar-hair" x={3} y={5} w={2} h={4} />
          <Px className="pixel-avatar-hair" x={11} y={5} w={2} h={4} />
        </>
      );
    case 'mohawk':
      return (
        <>
          <Px className="pixel-avatar-hair" x={7} y={1} w={2} h={4} />
          <Px className="pixel-avatar-hair" x={6} y={2} w={4} h={2} />
          <Px className="pixel-avatar-hair" x={4} y={4} w={8} h={2} />
          <Px className="pixel-avatar-hair" x={4} y={6} w={2} h={3} />
        </>
      );
    case 'long':
      return (
        <>
          <Px className="pixel-avatar-hair" x={4} y={3} w={8} h={3} />
          <Px className="pixel-avatar-hair" x={3} y={5} w={2} h={10} />
          <Px className="pixel-avatar-hair" x={11} y={5} w={2} h={10} />
          <Px className="pixel-avatar-hair" x={5} y={2} w={6} h={2} />
        </>
      );
    case 'cap':
      return (
        <>
          <Px className="pixel-avatar-hair" x={5} y={5} w={2} h={3} />
          <Px className="pixel-avatar-trim" x={4} y={3} w={8} h={3} />
          <Px className="pixel-avatar-trim" x={9} y={5} w={5} h={2} />
        </>
      );
    case 'bob':
      return (
        <>
          <Px className="pixel-avatar-hair" x={4} y={3} w={8} h={4} />
          <Px className="pixel-avatar-hair" x={3} y={6} w={3} h={6} />
          <Px className="pixel-avatar-hair" x={10} y={6} w={3} h={6} />
          <Px className="pixel-avatar-hair" x={5} y={2} w={6} h={2} />
        </>
      );
    case 'bun':
      return (
        <>
          <Px className="pixel-avatar-hair" x={6} y={1} w={4} h={3} />
          <Px className="pixel-avatar-hair" x={5} y={3} w={6} h={3} />
          <Px className="pixel-avatar-hair" x={4} y={5} w={2} h={4} />
          <Px className="pixel-avatar-hair" x={10} y={5} w={2} h={3} />
        </>
      );
  }
}

function SeatHair({ style }: { style: PixelHairStyle }): ReactNode {
  switch (style) {
    case 'crop':
      return (
        <>
          <Px className="pixel-avatar-ink" x={5} y={2} w={6} h={1} />
          <Px className="pixel-avatar-hair" x={4} y={2} w={8} h={4} />
          <Px className="pixel-avatar-hair" x={5} y={1} w={6} h={2} />
        </>
      );
    case 'curls':
      return (
        <>
          <Px className="pixel-avatar-hair" x={3} y={2} w={2} h={2} />
          <Px className="pixel-avatar-hair" x={5} y={1} w={2} h={2} />
          <Px className="pixel-avatar-hair" x={7} y={2} w={2} h={2} />
          <Px className="pixel-avatar-hair" x={9} y={1} w={2} h={3} />
          <Px className="pixel-avatar-hair" x={11} y={2} w={2} h={3} />
          <Px className="pixel-avatar-hair" x={3} y={4} w={10} h={3} />
          <Px className="pixel-avatar-hair" x={3} y={6} w={2} h={3} />
          <Px className="pixel-avatar-hair" x={11} y={6} w={2} h={3} />
        </>
      );
    case 'mohawk':
      return (
        <>
          <Px className="pixel-avatar-hair" x={7} y={0} w={2} h={4} />
          <Px className="pixel-avatar-hair" x={6} y={1} w={4} h={2} />
          <Px className="pixel-avatar-hair" x={4} y={3} w={8} h={3} />
        </>
      );
    case 'long':
      return (
        <>
          <Px className="pixel-avatar-hair" x={4} y={1} w={8} h={4} />
          <Px className="pixel-avatar-hair" x={3} y={4} w={2} h={8} />
          <Px className="pixel-avatar-hair" x={11} y={4} w={2} h={8} />
          <Px className="pixel-avatar-hair" x={5} y={2} w={6} h={2} />
        </>
      );
    case 'cap':
      return (
        <>
          <Px className="pixel-avatar-hair" x={5} y={4} w={2} h={2} />
          <Px className="pixel-avatar-trim" x={4} y={2} w={8} h={3} />
          <Px className="pixel-avatar-trim" x={8} y={4} w={6} h={2} />
        </>
      );
    case 'bob':
      return (
        <>
          <Px className="pixel-avatar-hair" x={4} y={2} w={8} h={4} />
          <Px className="pixel-avatar-hair" x={3} y={5} w={3} h={5} />
          <Px className="pixel-avatar-hair" x={10} y={5} w={3} h={5} />
          <Px className="pixel-avatar-hair" x={5} y={1} w={6} h={2} />
        </>
      );
    case 'bun':
      return (
        <>
          <Px className="pixel-avatar-hair" x={6} y={0} w={4} h={3} />
          <Px className="pixel-avatar-hair" x={5} y={2} w={6} h={3} />
          <Px className="pixel-avatar-hair" x={4} y={4} w={8} h={3} />
        </>
      );
  }
}

function BustAccessory({ type }: { type: PixelAccessory }): ReactNode {
  switch (type) {
    case 'overall':
      return (
        <>
          <Px className="pixel-avatar-trim" x={6} y={13} w={1} h={5} />
          <Px className="pixel-avatar-trim" x={9} y={13} w={1} h={5} />
          <Px className="pixel-avatar-trim" x={6} y={16} w={4} h={1} />
        </>
      );
    case 'scarf':
      return (
        <>
          <Px className="pixel-avatar-trim" x={5} y={12} w={6} h={2} />
          <Px className="pixel-avatar-trim" x={9} y={14} w={2} h={3} />
        </>
      );
    case 'tie':
      return (
        <>
          <Px className="pixel-avatar-trim" x={7} y={13} w={2} h={1} />
          <Px className="pixel-avatar-trim" x={7} y={14} w={1} h={3} />
        </>
      );
    case 'headphones':
      return (
        <>
          <Px className="pixel-avatar-trim" x={3} y={6} w={2} h={4} />
          <Px className="pixel-avatar-trim" x={11} y={6} w={2} h={4} />
          <Px className="pixel-avatar-trim" x={5} y={4} w={6} h={1} />
        </>
      );
    case 'glasses':
      return (
        <>
          <Px className="pixel-avatar-trim" x={5} y={8} w={3} h={2} />
          <Px className="pixel-avatar-trim" x={8} y={8} w={1} h={1} />
          <Px className="pixel-avatar-trim" x={9} y={8} w={3} h={2} />
        </>
      );
    case 'vest':
      return (
        <>
          <Px className="pixel-avatar-trim" x={5} y={13} w={2} h={5} />
          <Px className="pixel-avatar-trim" x={9} y={13} w={2} h={5} />
          <Px className="pixel-avatar-trim" x={7} y={13} w={2} h={1} />
        </>
      );
    default:
      return null;
  }
}

function SeatAccessory({ type }: { type: PixelAccessory }): ReactNode {
  switch (type) {
    case 'overall':
      return (
        <>
          <Px className="pixel-avatar-trim" x={6} y={11} w={1} h={3} />
          <Px className="pixel-avatar-trim" x={9} y={11} w={1} h={3} />
        </>
      );
    case 'scarf':
      return (
        <>
          <Px className="pixel-avatar-trim" x={5} y={9} w={6} h={2} />
          <Px className="pixel-avatar-trim" x={9} y={11} w={2} h={2} />
        </>
      );
    case 'tie':
      return <Px className="pixel-avatar-trim" x={7} y={11} w={2} h={2} />;
    case 'headphones':
      return (
        <>
          <Px className="pixel-avatar-trim" x={3} y={5} w={2} h={4} />
          <Px className="pixel-avatar-trim" x={11} y={5} w={2} h={4} />
          <Px className="pixel-avatar-trim" x={5} y={3} w={6} h={1} />
        </>
      );
    case 'glasses':
      return (
        <>
          <Px className="pixel-avatar-trim" x={5} y={7} w={3} h={2} />
          <Px className="pixel-avatar-trim" x={8} y={7} w={1} h={1} />
          <Px className="pixel-avatar-trim" x={9} y={7} w={3} h={2} />
        </>
      );
    case 'vest':
      return (
        <>
          <Px className="pixel-avatar-trim" x={4} y={11} w={2} h={3} />
          <Px className="pixel-avatar-trim" x={10} y={11} w={2} h={3} />
        </>
      );
    default:
      return null;
  }
}

function BustBody(): ReactNode {
  return (
    <>
      <Px className="pixel-avatar-shadow" x={4} y={21} w={8} h={1} />
      <Px className="pixel-avatar-shoe" x={5} y={20} w={3} h={1} />
      <Px className="pixel-avatar-shoe" x={8} y={20} w={3} h={1} />
      <Px className="pixel-avatar-pants" x={5} y={17} w={3} h={3} />
      <Px className="pixel-avatar-pants" x={8} y={17} w={3} h={3} />
      <Px className="pixel-avatar-ink" x={4} y={12} w={8} h={6} />
      <Px className="pixel-avatar-top" x={5} y={12} w={6} h={5} />
      <Px className="pixel-avatar-top" x={4} y={13} w={2} h={3} />
      <Px className="pixel-avatar-top" x={10} y={13} w={2} h={3} />
      <Px className="pixel-avatar-ink" x={4} y={5} w={8} h={8} />
      <Px className="pixel-avatar-face" x={5} y={6} w={6} h={6} />
      <Px className="pixel-avatar-face" x={6} y={12} w={4} h={1} />
      <Px className="pixel-avatar-eye" x={6} y={8} w={1} h={1} />
      <Px className="pixel-avatar-eye" x={9} y={8} w={1} h={1} />
      <Px className="pixel-avatar-mouth" x={7} y={10} w={2} h={1} />
    </>
  );
}

function SeatBody(): ReactNode {
  return (
    <>
      <Px className="pixel-avatar-chair" x={3} y={10} w={10} h={7} />
      <Px className="pixel-avatar-chair-seat" x={4} y={14} w={8} h={2} />
      <Px className="pixel-avatar-ink" x={4} y={10} w={8} h={5} />
      <Px className="pixel-avatar-top" x={5} y={10} w={6} h={4} />
      <Px className="pixel-avatar-top" x={3} y={11} w={2} h={3} />
      <Px className="pixel-avatar-top" x={11} y={11} w={2} h={3} />
      <Px className="pixel-avatar-ink" x={4} y={3} w={8} h={8} />
      <Px className="pixel-avatar-face" x={5} y={4} w={6} h={6} />
      <Px className="pixel-avatar-face" x={6} y={10} w={4} h={1} />
      <Px className="pixel-avatar-eye" x={6} y={6} w={1} h={1} />
      <Px className="pixel-avatar-eye" x={9} y={6} w={1} h={1} />
      <Px className="pixel-avatar-mouth" x={7} y={8} w={2} h={1} />
    </>
  );
}

function WalkBody({ facing }: { facing: PixelAvatarFacing }): ReactNode {
  const side = facing === 'east' || facing === 'west';
  const north = facing === 'north';
  return (
    <>
      <Px className="pixel-avatar-shadow" x={4} y={21} w={8} h={1} />
      <g className="pixel-avatar-leg-a">
        <Px className="pixel-avatar-pants" x={side ? 6 : 5} y={17} w={3} h={3} />
        <Px className="pixel-avatar-shoe" x={side ? 6 : 5} y={20} w={3} h={1} />
      </g>
      <g className="pixel-avatar-leg-b">
        <Px className="pixel-avatar-pants" x={side ? 8 : 8} y={17} w={3} h={3} />
        <Px className="pixel-avatar-shoe" x={side ? 9 : 8} y={20} w={3} h={1} />
      </g>
      <Px className="pixel-avatar-ink" x={4} y={12} w={8} h={6} />
      <Px className="pixel-avatar-top" x={5} y={12} w={6} h={5} />
      <Px className="pixel-avatar-top" x={4} y={13} w={2} h={3} />
      <Px className="pixel-avatar-top" x={10} y={13} w={2} h={3} />
      <Px className="pixel-avatar-ink" x={4} y={5} w={8} h={8} />
      {!north && <Px className="pixel-avatar-face" x={5} y={6} w={6} h={6} />}
      {!north && <Px className="pixel-avatar-face" x={6} y={12} w={4} h={1} />}
      {!north && !side && (
        <>
          <Px className="pixel-avatar-eye" x={6} y={8} w={1} h={1} />
          <Px className="pixel-avatar-eye" x={9} y={8} w={1} h={1} />
          <Px className="pixel-avatar-mouth" x={7} y={10} w={2} h={1} />
        </>
      )}
      {side && (
        <>
          <Px className="pixel-avatar-eye" x={9} y={8} w={1} h={1} />
          <Px className="pixel-avatar-mouth" x={9} y={10} w={2} h={1} />
        </>
      )}
    </>
  );
}

export function PixelAvatar({
  avatarId,
  animated = false,
  pose = 'bust',
  facing = 'south',
  className = '',
}: {
  avatarId: string;
  animated?: boolean;
  pose?: PixelAvatarPose;
  facing?: PixelAvatarFacing;
  className?: string;
}) {
  const profile = PIXEL_AVATARS.find(item => item.id === avatarId) ?? PIXEL_AVATARS[0];
  const style = {
    '--pixel-skin': profile.skin,
    '--pixel-hair': profile.hair,
    '--pixel-top': profile.top,
    '--pixel-trim': profile.trim,
    '--pixel-pants': profile.pants,
    '--pixel-shoes': profile.shoes,
  } as CSSProperties;
  const seated = pose === 'seat';
  const walking = pose === 'walk';
  const hairFacing = walking && facing === 'north' ? 'seat' : 'bust';

  return (
    <svg
      className={`pixel-avatar${seated ? ' is-seat' : walking ? ' is-walk' : ' is-bust'}${animated ? ' is-idle' : ''}${walking ? ' is-stepping' : ''}${facing === 'west' ? ' is-mirror' : ''}${className ? ` ${className}` : ''}`}
      viewBox={seated ? '0 0 16 18' : '0 0 16 22'}
      shapeRendering="crispEdges"
      style={style}
      role="img"
      aria-label={profile.name}
    >
      <g className="pixel-avatar-person">
        {seated ? <SeatBody /> : walking ? <WalkBody facing={facing} /> : <BustBody />}
        <g className="pixel-avatar-hair">
          {hairFacing === 'seat'
            ? <SeatHair style={profile.hairStyle} />
            : <BustHair style={profile.hairStyle} />}
        </g>
        {seated
          ? <SeatAccessory type={profile.accessory} />
          : <BustAccessory type={profile.accessory} />}
      </g>
    </svg>
  );
}
