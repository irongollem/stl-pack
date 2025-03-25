export type StlModel = {
  modelName: string;
  description: string;
  release?: string;
  releaseDate?: string; // MM/YYYY
  designer?: string;
  tags: string[];
  images: string[];
  modelFiles: string[];
};

export type FileContext = {
  name: string;
  path: string;
  file: File;
};

export const fileLogos = {
  chitubox: "/chitubox.jpg",
  lychee: "/lychee.jpg",
  stl: "/stl.jpg",
};
